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
impl X86 {
    /// Linux rt_sigframe header: restorer, ucontext, siginfo. No FP state yet.
    pub fn enter_signal(
        &mut self,
        memory: &mut Memory,
        signal: u32,
        handler: u64,
        restorer: u64,
        mask: u64,
        sender: u32,
    ) -> Result<()> {
        let sp = (self.regs[4].checked_sub(128).ok_or(Error::ResourceLimit)? & !15)
            .checked_sub(440)
            .ok_or(Error::ResourceLimit)?;
        let mut frame = [0u8; 440];
        fn put(frame: &mut [u8], off: usize, value: u64) {
            frame[off..off + 8].copy_from_slice(&value.to_le_bytes());
        }
        put(&mut frame, 0, restorer);
        put(&mut frame, UC + 24, 2);
        for (n, index) in ORDER.iter().enumerate() {
            put(&mut frame, UC + 40 + n * 8, self.regs[*index]);
        }
        put(&mut frame, UC + 168, self.rip);
        put(&mut frame, UC + 176, self.flags);
        put(&mut frame, UC + 184, 0x002b_0000_0000_0033);
        put(&mut frame, UC + 208, mask);
        put(&mut frame, UC + 296, mask);
        put(&mut frame, INFO, signal as u64);
        put(&mut frame, INFO + 16, sender as u64);
        memory.write(sp, &frame)?;
        self.regs[4] = sp;
        self.regs[7] = signal as u64;
        self.regs[6] = sp + INFO as u64;
        self.regs[2] = sp + UC as u64;
        self.rip = handler;
        self.flags &= !1024;
        Ok(())
    }
    pub fn restore_signal(&mut self, memory: &Memory) -> Result<u64> {
        let sp = self.regs[4].checked_sub(8).ok_or(Error::ResourceLimit)?;
        let mut frame = [0u8; 440];
        memory.read(sp, &mut frame, Access::Read)?;
        let get = |off: usize| u64::from_le_bytes(frame[off..off + 8].try_into().unwrap());
        if get(UC + 184) != 0x002b_0000_0000_0033 {
            return Err(Error::InvalidConfig(
                "unsupported signal segment selectors".into(),
            ));
        }
        for (n, index) in ORDER.iter().enumerate() {
            self.regs[*index] = get(UC + 40 + n * 8);
        }
        self.rip = get(UC + 168);
        self.flags = (get(UC + 176) & 0xcd5) | 2;
        Ok(get(UC + 296))
    }
}
