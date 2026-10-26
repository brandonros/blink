// Linux integer signal context layout and save/restore order adapted from
// blink/linux.h and blink/signal.c.
// Copyright 2022 Justine Alexandra Roberts Tunney
//
// Permission to use, copy, modify, and/or distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
// ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
// ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
// OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
use super::X86;
use blink_core::{Access, Error, Memory, Result};
const ORDER: [usize; 16] = [8, 9, 10, 11, 12, 13, 14, 15, 7, 6, 5, 3, 2, 0, 1, 4];
const UC: usize = 8;
const INFO: usize = 312;
#[derive(Clone, Copy, Debug)]
pub struct SignalStack {
    pub pointer: u64,
    pub size: u64,
    pub flags: u32,
}
impl Default for SignalStack {
    fn default() -> Self {
        Self {
            pointer: 0,
            size: 0,
            flags: 2,
        }
    }
}
impl SignalStack {
    pub fn contains(self, sp: u64) -> bool {
        self.flags & 2 == 0 && sp > self.pointer && sp.wrapping_sub(self.pointer) <= self.size
    }
    pub fn status(self, sp: u64) -> u32 {
        if self.flags & 2 != 0 {
            self.flags | 2
        } else if self.flags & 0x80000000 != 0 {
            0x80000000
        } else if self.contains(sp) {
            1
        } else {
            0
        }
    }
}
#[derive(Clone, Copy, Default)]
pub struct SignalInfo {
    pub sender: u32,
    pub uid: u32,
    pub code: i32,
    pub status: i32,
    pub address: Option<u64>,
    pub trap: u64,
}
pub struct SignalContext {
    pub signal: u32,
    pub handler: u64,
    pub restorer: u64,
    pub mask: u64,
    pub info: SignalInfo,
    pub stack: SignalStack,
    pub use_stack: bool,
}
impl X86 {
    /// Linux rt_sigframe header: restorer, ucontext, siginfo. Legacy FP area preserves the implemented x87 and XMM registers.
    pub fn enter_signal(
        &mut self,
        memory: &mut Memory,
        signal: u32,
        handler: u64,
        restorer: u64,
        mask: u64,
        sender: u32,
    ) -> Result<()> {
        self.enter_signal_context(
            memory,
            SignalContext {
                signal,
                handler,
                restorer,
                mask,
                info: SignalInfo {
                    sender,
                    ..Default::default()
                },
                stack: SignalStack::default(),
                use_stack: false,
            },
        )
    }
    pub fn enter_signal_context(
        &mut self,
        memory: &mut Memory,
        context: SignalContext,
    ) -> Result<()> {
        let SignalContext {
            signal,
            handler,
            restorer,
            mask,
            info,
            stack,
            use_stack,
        } = context;
        let switching = use_stack && stack.flags & 2 == 0 && stack.status(self.regs[4]) != 1;
        let top = if switching {
            stack
                .pointer
                .checked_add(stack.size)
                .ok_or(Error::ResourceLimit)?
        } else {
            self.regs[4].checked_sub(128).ok_or(Error::ResourceLimit)?
        };
        let sp = (top & !15).checked_sub(952).ok_or(Error::ResourceLimit)?;
        if (switching || stack.contains(self.regs[4])) && !stack.contains(sp) {
            return Err(Error::ResourceLimit);
        }
        let mut frame = [0u8; 952];
        fn put(frame: &mut [u8], off: usize, value: u64) {
            frame[off..off + 8].copy_from_slice(&value.to_le_bytes());
        }
        put(&mut frame, 0, restorer);
        put(&mut frame, UC + 16, stack.pointer);
        put(&mut frame, UC + 24, stack.flags as u64);
        put(&mut frame, UC + 32, stack.size);
        for (n, index) in ORDER.iter().enumerate() {
            put(&mut frame, UC + 40 + n * 8, self.regs[*index]);
        }
        put(&mut frame, UC + 168, self.rip);
        put(&mut frame, UC + 176, self.flags);
        put(&mut frame, UC + 184, 0x002b_0000_0000_0033);
        put(&mut frame, UC + 208, mask);
        put(&mut frame, UC + 296, mask);
        put(&mut frame, INFO, signal as u64);
        frame[INFO + 8..INFO + 12].copy_from_slice(&info.code.to_le_bytes());
        if let Some(address) = info.address {
            put(&mut frame, INFO + 16, address);
            put(&mut frame, UC + 200, info.trap);
        } else {
            frame[INFO + 16..INFO + 20].copy_from_slice(&info.sender.to_le_bytes());
            frame[INFO + 20..INFO + 24].copy_from_slice(&info.uid.to_le_bytes());
            frame[INFO + 24..INFO + 28].copy_from_slice(&info.status.to_le_bytes());
        }
        put(&mut frame, UC + 224, sp + 440);
        self.fpu.save_legacy(&mut frame[440..]);
        frame[464..468].copy_from_slice(&self.mxcsr.to_le_bytes());
        frame[468..472].copy_from_slice(&0xffffu32.to_le_bytes());
        for n in 0..16 {
            frame[600 + n * 16..616 + n * 16].copy_from_slice(&self.xmm[n]);
        }
        memory.write(sp, &frame)?;
        self.regs[4] = sp;
        self.regs[7] = signal as u64;
        self.regs[6] = sp + INFO as u64;
        self.regs[2] = sp + UC as u64;
        self.rip = handler;
        self.flags &= !1024;
        self.mxcsr = 0x1f80;
        self.fpu = super::x87::Fpu::default();
        Ok(())
    }
    pub fn restore_signal(&mut self, memory: &Memory) -> Result<u64> {
        self.restore_signal_context(memory).map(|(mask, _)| mask)
    }
    pub fn restore_signal_context(&mut self, memory: &Memory) -> Result<(u64, SignalStack)> {
        let sp = self.regs[4].checked_sub(8).ok_or(Error::ResourceLimit)?;
        let mut frame = [0u8; 440];
        memory.read(sp, &mut frame, Access::Read)?;
        let get = |off: usize| u64::from_le_bytes(frame[off..off + 8].try_into().unwrap());
        if get(UC + 184) != 0x002b_0000_0000_0033 {
            return Err(Error::InvalidConfig(
                "unsupported signal segment selectors".into(),
            ));
        }
        let pointer = get(UC + 224);
        let mut xmm = [[0; 16]; 16];
        let mut mxcsr = 0x1f80;
        let mut fpu = super::x87::Fpu::default();
        if pointer != 0 {
            let mut legacy = [0; 160];
            memory.read(pointer, &mut legacy, Access::Read)?;
            fpu = super::x87::Fpu::restore_legacy(&legacy);
            mxcsr = memory.load(pointer.checked_add(24).ok_or(Error::ResourceLimit)?, 4)? as u32;
            if mxcsr & !0xffff != 0 {
                return Err(Error::InvalidConfig(
                    "reserved MXCSR bits in signal context".into(),
                ));
            }
            for (n, slot) in xmm.iter_mut().enumerate() {
                memory.read(
                    pointer
                        .checked_add(160 + n as u64 * 16)
                        .ok_or(Error::ResourceLimit)?,
                    slot,
                    Access::Read,
                )?;
            }
        }
        self.xmm = xmm;
        self.fpu = fpu;
        self.mxcsr = mxcsr;
        for (n, index) in ORDER.iter().enumerate() {
            self.regs[*index] = get(UC + 40 + n * 8);
        }
        self.rip = get(UC + 168);
        self.flags = (get(UC + 176) & 0xcd5) | 2;
        Ok((
            get(UC + 296),
            SignalStack {
                pointer: get(UC + 16),
                flags: get(UC + 24) as u32,
                size: get(UC + 32),
            },
        ))
    }
}
