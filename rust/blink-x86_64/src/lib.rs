//! AMD64 integer interpreter with a bounded decoder adapter.
mod decode;
mod float;
mod packed;
mod signal;
mod x87;
use blink_core::{Cpu, Error, Memory, Result, Step};
use decode::Value;
pub use signal::{SignalContext, SignalInfo, SignalStack};
use yaxpeax_x86::long_mode::{ConditionCode as C, Opcode as Op};
#[derive(Clone)]
pub struct X86 {
    pub regs: [u64; 16],
    pub rip: u64,
    pub flags: u64,
    pub fs_base: u64,
    pub xmm: [[u8; 16]; 16],
    pub mxcsr: u32,
    pub(crate) fpu: x87::Fpu,
    pub ticks: u64,
}
fn mask(w: usize) -> u64 {
    if w == 8 {
        u64::MAX
    } else {
        (1u64 << (w * 8)) - 1
    }
}
fn signed(v: u64, w: usize) -> i128 {
    ((v << (64 - w * 8)) as i64 >> (64 - w * 8)) as i128
}
impl X86 {
    pub fn new(entry: u64, stack: u64) -> Self {
        let mut c = Self {
            regs: [0; 16],
            rip: entry,
            flags: 2,
            fs_base: 0,
            xmm: [[0; 16]; 16],
            mxcsr: 0x1f80,
            fpu: x87::Fpu::default(),
            ticks: 0,
        };
        c.regs[4] = stack;
        c
    }
    fn read(&self, m: &Memory, v: Value) -> Result<u64> {
        Ok(match v {
            Value::Immediate(v) => v,
            Value::Vector(_) | Value::Mmx(_) | Value::FloatStack(_) => {
                return Err(Error::UnsupportedInstruction {
                    pc: self.rip,
                    encoding: 0,
                })
            }
            Value::Register {
                index,
                width,
                shift,
            } => (self.regs[index] >> shift) & mask(width),
            Value::Memory { address, width } => m.load(address, width)?,
        })
    }
    fn write(&mut self, m: &mut Memory, dst: Value, v: u64) -> Result<()> {
        match dst {
            Value::Register {
                index,
                width,
                shift,
            } => {
                let v = v & mask(width);
                self.regs[index] = if width >= 4 {
                    v
                } else {
                    (self.regs[index] & !(mask(width) << shift)) | (v << shift)
                };
                Ok(())
            }
            Value::Memory { address, width } => m.store(address, v, width),
            _ => Err(Error::UnsupportedInstruction {
                pc: self.rip,
                encoding: 0,
            }),
        }
    }
    fn vector(&self, m: &Memory, value: Value) -> Result<[u8; 16]> {
        let mut bytes = [0; 16];
        match value {
            Value::Vector(i) => bytes = self.xmm[i],
            Value::Mmx(i) => bytes[..8].copy_from_slice(&(self.fpu.values[i] as u64).to_le_bytes()),
            Value::Memory { address, width } => {
                m.read(address, &mut bytes[..width], blink_core::Access::Read)?
            }
            _ => {
                let v = self.read(m, value)?;
                let n = value.width().min(8);
                bytes[..n].copy_from_slice(&v.to_le_bytes()[..n]);
            }
        }
        Ok(bytes)
    }
    fn write_vector(&mut self, m: &mut Memory, dst: Value, bytes: [u8; 16]) -> Result<()> {
        match dst {
            Value::Mmx(i) => {
                self.fpu.values[i] =
                    (0xffffu128 << 64) | u64::from_le_bytes(bytes[..8].try_into().unwrap()) as u128;
                Ok(())
            }
            Value::Vector(i) => {
                self.xmm[i] = bytes;
                Ok(())
            }
            Value::Memory { address, width } => m.write(address, &bytes[..width]),
            _ => self.write(m, dst, u64::from_le_bytes(bytes[..8].try_into().unwrap())),
        }
    }
    fn arithmetic(&mut self, a: u64, b: u64, w: usize, sub: bool, carry: bool) -> u64 {
        let mask = mask(w);
        let sign = 1u64 << (w * 8 - 1);
        let a = a & mask;
        let b = b & mask;
        let c = carry as u64;
        let v = if sub {
            a.wrapping_sub(b).wrapping_sub(c)
        } else {
            a.wrapping_add(b).wrapping_add(c)
        } & mask;
        let cf = if sub {
            (a as u128) < b as u128 + c as u128
        } else {
            a as u128 + b as u128 + c as u128 > mask as u128
        };
        let of = if sub {
            (a ^ b) & (a ^ v) & sign != 0
        } else {
            !(a ^ b) & (a ^ v) & sign != 0
        };
        self.flags = (self.flags & !0x8d5)
            | 2
            | cf as u64
            | (((v as u8).count_ones() % 2 == 0) as u64) << 2
            | (((a ^ b ^ v) & 16 != 0) as u64) << 4
            | ((v == 0) as u64) << 6
            | ((v & sign != 0) as u64) << 7
            | (of as u64) << 11;
        v
    }
    fn logical(&mut self, v: u64, w: usize) -> u64 {
        let v = v & mask(w);
        self.flags = (self.flags & !0x8d5)
            | 2
            | (((v as u8).count_ones() % 2 == 0) as u64) << 2
            | ((v == 0) as u64) << 6
            | ((v & (1 << (w * 8 - 1)) != 0) as u64) << 7;
        v
    }
    fn condition(&self, c: C) -> bool {
        let o = self.flags & 2048 != 0;
        let s = self.flags & 128 != 0;
        let z = self.flags & 64 != 0;
        let p = self.flags & 4 != 0;
        let b = self.flags & 1 != 0;
        match c {
            C::O => o,
            C::NO => !o,
            C::B => b,
            C::AE => !b,
            C::Z => z,
            C::NZ => !z,
            C::A => !b && !z,
            C::BE => b || z,
            C::S => s,
            C::NS => !s,
            C::P => p,
            C::NP => !p,
            C::L => s != o,
            C::GE => s == o,
            C::G => !z && s == o,
            C::LE => z || s != o,
        }
    }
    fn reg(index: usize, width: usize) -> Value {
        Value::Register {
            index,
            width,
            shift: 0,
        }
    }
}
impl Cpu for X86 {
    fn set_ticks(&mut self, ticks: u64) {
        self.ticks = ticks;
    }
    fn pc(&self) -> u64 {
        self.rip
    }
    fn set_result(&mut self, v: u64) {
        self.regs[0] = v;
    }
    fn step(&mut self, m: &mut Memory) -> Result<Step> {
        let pc = self.rip;
        let (ins, next) = decode::instruction(m, pc)?;
        let op = ins.opcode();
        // Hints never access memory, even if their decoder operand describes a
        // cache line rather than one of our scalar/vector load widths.
        // Memory is interpreted sequentially, so fences need no host operation.
        if matches!(
            op,
            Op::NOP
                | Op::ENDBR64
                | Op::PREFETCHNTA
                | Op::PREFETCH0
                | Op::PREFETCH1
                | Op::PREFETCH2
                | Op::LFENCE
                | Op::SFENCE
                | Op::MFENCE
        ) {
            self.rip = next;
            return Ok(Step::Continue);
        }
        let bad = || Error::UnsupportedInstruction {
            pc,
            encoding: op as u32,
        };
        let a = if ins.operand_count() > 0 {
            Some(decode::operand(self, &ins, 0, next, op == Op::LEA)?)
        } else {
            None
        };
        let b = if ins.operand_count() > 1 {
            Some(decode::operand(self, &ins, 1, next, op == Op::LEA)?)
        } else {
            None
        };
        let c = if ins.operand_count() > 2 {
            Some(decode::operand(self, &ins, 2, next, false)?)
        } else {
            None
        };
        if op == Op::CLFLUSH {
            // Guest memory has no data/instruction cache. Still enforce the
            // addressed byte's read permission, unlike a prefetch hint.
            self.read(m, a.ok_or_else(bad)?)?;
            self.rip = next;
            return Ok(Step::Continue);
        }
        let mmx = [a, b, c]
            .into_iter()
            .flatten()
            .any(|v| matches!(v, Value::Mmx(_)));
        if (mmx || op == Op::EMMS) && self.fpu.status & !self.fpu.control & 63 != 0 {
            return Err(Error::ArithmeticFault);
        }
        if op == Op::EMMS {
            self.fpu.occupied = [false; 8];
            self.rip = next;
            return Ok(Step::Continue);
        }
        // Legacy 128-bit SSE memory operands require 16-byte alignment except
        // the explicitly unaligned moves. Scalar and 64-bit operands do not.
        if [a, b, c]
            .into_iter()
            .flatten()
            .any(|v| matches!(v, Value::Vector(_)))
            && !matches!(op, Op::MOVUPS | Op::MOVUPD | Op::MOVDQU)
        {
            for value in [a, b, c].into_iter().flatten() {
                if let Value::Memory { address, width: 16 } = value {
                    if address & 15 != 0 {
                        return Err(Error::MemoryFault {
                            address,
                            access: blink_core::Access::Read,
                        });
                    }
                }
            }
        }
        self.rip = next;
        if matches!(op, Op::FXSAVE | Op::FXRSTOR) {
            let Value::Memory { address, .. } = a.ok_or_else(bad)? else {
                return Err(bad());
            };
            self.fxstate(
                m,
                address,
                op == Op::FXRSTOR,
                ins.prefixes.rex().is_some_and(|r| r.w()),
            )?;
            return Ok(Step::Continue);
        }
        if self.x87(m, op, a, b, pc)?
            || self.float(m, op, a, b, c, pc)?
            || self.packed(m, op, a, b, c, pc)?
        {
            if mmx {
                self.fpu.occupied = [true; 8];
                self.fpu.top = 0;
            }
            return Ok(Step::Continue);
        }
        match op {
            Op::MASKMOVQ | Op::MASKMOVDQU => {
                let data = a.ok_or_else(bad)?;
                let bytes = self.vector(m, data)?;
                let mask = self.vector(m, b.ok_or_else(bad)?)?;
                let offset = if ins.prefixes.address_size() {
                    self.regs[7] as u32 as u64
                } else {
                    self.regs[7]
                };
                let segment = match ins.prefixes.segment() {
                    yaxpeax_x86::long_mode::Segment::FS => self.fs_base,
                    yaxpeax_x86::long_mode::Segment::GS => return Err(bad()),
                    _ => 0,
                };
                let address = offset.wrapping_add(segment);
                for (n, byte) in mask[..data.width()].iter().enumerate() {
                    if byte & 128 != 0 {
                        m.check(address.wrapping_add(n as u64), 1, blink_core::Access::Write)?;
                    }
                }
                for n in 0..data.width() {
                    if mask[n] & 128 != 0 {
                        m.store(address.wrapping_add(n as u64), bytes[n] as u64, 1)?;
                    }
                }
            }
            Op::MOVHPS | Op::MOVHPD | Op::MOVLPS | Op::MOVLPD | Op::MOVHLPS | Op::MOVLHPS => {
                let dst = a.ok_or_else(bad)?;
                let src = b.ok_or_else(bad)?;
                let bytes = self.vector(m, src)?;
                let high = matches!(op, Op::MOVHPS | Op::MOVHPD | Op::MOVLHPS);
                if matches!(dst, Value::Memory { .. }) {
                    let mut out = [0; 16];
                    out[..8].copy_from_slice(&bytes[if high { 8..16 } else { 0..8 }]);
                    self.write_vector(m, dst, out)?;
                } else {
                    let mut out = self.vector(m, dst)?;
                    let source = if op == Op::MOVHLPS { 8..16 } else { 0..8 };
                    out[if high { 8..16 } else { 0..8 }].copy_from_slice(&bytes[source]);
                    self.write_vector(m, dst, out)?;
                }
            }
            Op::MOVD
            | Op::MOVQ
            | Op::MOVQ2DQ
            | Op::MOVDQ2Q
            | Op::MOVNTQ
            | Op::MOVDQA
            | Op::MOVDQU
            | Op::MOVAPS
            | Op::MOVUPS
            | Op::MOVAPD
            | Op::MOVUPD
            | Op::MOVSS
            | Op::MOVSD
            | Op::MOVNTDQ
            | Op::MOVNTPS
            | Op::MOVNTPD => {
                let dst = a.ok_or_else(bad)?;
                let src = b.ok_or_else(bad)?;
                if matches!(op, Op::MOVDQA | Op::MOVAPS | Op::MOVAPD) {
                    for value in [dst, src] {
                        if let Value::Memory { address, .. } = value {
                            if address & 15 != 0 {
                                return Err(Error::MemoryFault {
                                    address,
                                    access: blink_core::Access::Read,
                                });
                            }
                        }
                    }
                }
                let mut bytes = self.vector(m, src)?;
                let width = match op {
                    Op::MOVD | Op::MOVSS => 4,
                    Op::MOVQ | Op::MOVSD | Op::MOVQ2DQ | Op::MOVDQ2Q | Op::MOVNTQ => 8,
                    _ => 16,
                };
                if width < 16 {
                    let old = if matches!(op, Op::MOVSS | Op::MOVSD)
                        && matches!(src, Value::Vector(_))
                        && matches!(dst, Value::Vector(_))
                    {
                        self.vector(m, dst)?
                    } else {
                        [0; 16]
                    };
                    bytes[width..].copy_from_slice(&old[width..]);
                }
                self.write_vector(m, dst, bytes)?;
            }
            Op::PMINUB
            | Op::PMAXUB
            | Op::PXOR
            | Op::XORPS
            | Op::XORPD
            | Op::PAND
            | Op::PANDN
            | Op::POR
            | Op::PCMPGTB
            | Op::PCMPGTW
            | Op::PCMPGTD
            | Op::PCMPEQW
            | Op::PCMPEQB
            | Op::PCMPEQD
            | Op::PCMPEQQ
            | Op::PADDB
            | Op::PADDW
            | Op::PSUBB
            | Op::PSUBW
            | Op::PADDQ
            | Op::PADDD
            | Op::PSUBQ
            | Op::PSUBD => {
                let dst = a.ok_or_else(bad)?;
                let left = self.vector(m, dst)?;
                let right = self.vector(m, b.ok_or_else(bad)?)?;
                let mut out = [0; 16];
                match op {
                    Op::PCMPEQB => {
                        for n in 0..16 {
                            out[n] = if left[n] == right[n] { 255 } else { 0 };
                        }
                    }
                    Op::PCMPGTB | Op::PCMPGTW | Op::PCMPGTD => {
                        let lane = match op {
                            Op::PCMPGTB => 1,
                            Op::PCMPGTW => 2,
                            _ => 4,
                        };
                        for n in (0..16).step_by(lane) {
                            let mut l = [0; 8];
                            let mut r = [0; 8];
                            l[..lane].copy_from_slice(&left[n..n + lane]);
                            r[..lane].copy_from_slice(&right[n..n + lane]);
                            out[n..n + lane].fill(
                                if signed(u64::from_le_bytes(l), lane)
                                    > signed(u64::from_le_bytes(r), lane)
                                {
                                    255
                                } else {
                                    0
                                },
                            );
                        }
                    }
                    Op::PCMPEQW | Op::PCMPEQD | Op::PCMPEQQ => {
                        let lane = if op == Op::PCMPEQW {
                            2
                        } else if op == Op::PCMPEQD {
                            4
                        } else {
                            8
                        };
                        for n in (0..16).step_by(lane) {
                            out[n..n + lane].fill(if left[n..n + lane] == right[n..n + lane] {
                                255
                            } else {
                                0
                            });
                        }
                    }
                    Op::PADDB
                    | Op::PADDW
                    | Op::PSUBB
                    | Op::PSUBW
                    | Op::PADDD
                    | Op::PSUBD
                    | Op::PADDQ
                    | Op::PSUBQ => {
                        let lane = if matches!(op, Op::PADDB | Op::PSUBB) {
                            1
                        } else if matches!(op, Op::PADDW | Op::PSUBW) {
                            2
                        } else if matches!(op, Op::PADDD | Op::PSUBD) {
                            4
                        } else {
                            8
                        };
                        for n in (0..16).step_by(lane) {
                            let mut l = [0; 8];
                            let mut r = [0; 8];
                            l[..lane].copy_from_slice(&left[n..n + lane]);
                            r[..lane].copy_from_slice(&right[n..n + lane]);
                            let l = u64::from_le_bytes(l);
                            let r = u64::from_le_bytes(r);
                            let v = if matches!(op, Op::PADDB | Op::PADDW | Op::PADDD | Op::PADDQ) {
                                l.wrapping_add(r)
                            } else {
                                l.wrapping_sub(r)
                            };
                            out[n..n + lane].copy_from_slice(&v.to_le_bytes()[..lane]);
                        }
                    }
                    _ => {
                        for n in 0..16 {
                            out[n] = match op {
                                Op::PMINUB => left[n].min(right[n]),
                                Op::PMAXUB => left[n].max(right[n]),
                                Op::PAND => left[n] & right[n],
                                Op::PANDN => !left[n] & right[n],
                                Op::POR => left[n] | right[n],
                                _ => left[n] ^ right[n],
                            };
                        }
                    }
                }
                self.write_vector(m, dst, out)?;
            }
            Op::PUNPCKLBW
            | Op::PUNPCKLWD
            | Op::PUNPCKHBW
            | Op::PUNPCKHWD
            | Op::PUNPCKLDQ
            | Op::PUNPCKLQDQ
            | Op::PUNPCKHDQ
            | Op::PUNPCKHQDQ => {
                let dst = a.ok_or_else(bad)?;
                let left = self.vector(m, dst)?;
                let right = self.vector(m, b.ok_or_else(bad)?)?;
                let lane = if matches!(op, Op::PUNPCKLBW | Op::PUNPCKHBW) {
                    1
                } else if matches!(op, Op::PUNPCKLWD | Op::PUNPCKHWD) {
                    2
                } else if matches!(op, Op::PUNPCKLDQ | Op::PUNPCKHDQ) {
                    4
                } else {
                    8
                };
                let start = if matches!(
                    op,
                    Op::PUNPCKHBW | Op::PUNPCKHWD | Op::PUNPCKHDQ | Op::PUNPCKHQDQ
                ) {
                    dst.width() / 2
                } else {
                    0
                };
                let mut out = [0; 16];
                for n in 0..dst.width() / 2 / lane {
                    out[n * lane * 2..n * lane * 2 + lane]
                        .copy_from_slice(&left[start + n * lane..start + n * lane + lane]);
                    out[n * lane * 2 + lane..n * lane * 2 + lane * 2]
                        .copy_from_slice(&right[start + n * lane..start + n * lane + lane]);
                }
                self.write_vector(m, dst, out)?;
            }
            Op::PSHUFD | Op::PSHUFW => {
                let src = self.vector(m, b.ok_or_else(bad)?)?;
                let imm = self.read(m, c.ok_or_else(bad)?)?;
                let mut out = [0; 16];
                let width = if op == Op::PSHUFD { 4 } else { 2 };
                for n in 0..4 {
                    let index = ((imm >> (n * 2)) & 3) as usize;
                    out[n * width..n * width + width]
                        .copy_from_slice(&src[index * width..index * width + width]);
                }
                self.write_vector(m, a.ok_or_else(bad)?, out)?;
            }
            Op::PSLLDQ | Op::PSRLDQ => {
                let dst = a.ok_or_else(bad)?;
                let src = self.vector(m, dst)?;
                let count = self.read(m, b.ok_or_else(bad)?)?.min(16) as usize;
                let mut out = [0; 16];
                if op == Op::PSLLDQ {
                    out[count..].copy_from_slice(&src[..16 - count]);
                } else {
                    out[..16 - count].copy_from_slice(&src[count..]);
                }
                self.write_vector(m, dst, out)?;
            }
            Op::PSLLQ | Op::PSRLQ => {
                let dst = a.ok_or_else(bad)?;
                let src = self.vector(m, dst)?;
                let raw = self.vector(m, b.ok_or_else(bad)?)?;
                let count = u64::from_le_bytes(raw[..8].try_into().unwrap());
                let mut out = [0; 16];
                for n in [0, 8] {
                    let value = u64::from_le_bytes(src[n..n + 8].try_into().unwrap());
                    let result = if count >= 64 {
                        0
                    } else if op == Op::PSLLQ {
                        value << count
                    } else {
                        value >> count
                    };
                    out[n..n + 8].copy_from_slice(&result.to_le_bytes());
                }
                self.write_vector(m, dst, out)?;
            }
            Op::PMOVMSKB => {
                let src = self.vector(m, b.ok_or_else(bad)?)?;
                let mut bits = 0;
                for (n, v) in src.iter().enumerate() {
                    bits |= ((v >> 7) as u64) << n;
                }
                self.write(m, a.ok_or_else(bad)?, bits)?;
            }
            Op::MOVS | Op::STOS | Op::LODS | Op::CMPS | Op::SCAS => {
                let repeated = ins.prefixes.rep_any();
                let address_width = if ins.prefixes.address_size() { 4 } else { 8 };
                let count = self.regs[1] & mask(address_width);
                if !repeated || count != 0 {
                    let dst = a.ok_or_else(bad)?;
                    let src = b.ok_or_else(bad)?;
                    let w = dst.width();
                    if matches!(op, Op::CMPS | Op::SCAS) {
                        let left = self.read(m, dst)?;
                        let right = self.read(m, src)?;
                        self.arithmetic(left, right, w, true, false);
                    } else {
                        let v = self.read(m, src)?;
                        self.write(m, dst, v)?;
                    }
                    let delta = if self.flags & 1024 != 0 {
                        (0u64).wrapping_sub(w as u64)
                    } else {
                        w as u64
                    };
                    if matches!(op, Op::MOVS | Op::LODS | Op::CMPS) {
                        self.regs[6] = self.regs[6].wrapping_add(delta) & mask(address_width);
                    }
                    if matches!(op, Op::MOVS | Op::STOS | Op::CMPS | Op::SCAS) {
                        self.regs[7] = self.regs[7].wrapping_add(delta) & mask(address_width);
                    }
                    if repeated {
                        self.regs[1] = count - 1;
                        let keep_going = !matches!(op, Op::CMPS | Op::SCAS)
                            || ((self.flags & 64 == 0) == ins.prefixes.repnz());
                        if count > 1 && keep_going {
                            self.rip = pc;
                        }
                    }
                }
            }
            Op::SYSCALL => {
                self.regs[1] = next;
                self.regs[11] = self.flags;
                return Ok(Step::Syscall {
                    number: self.regs[0],
                    args: [
                        self.regs[7],
                        self.regs[6],
                        self.regs[2],
                        self.regs[10],
                        self.regs[8],
                        self.regs[9],
                    ],
                });
            }
            Op::MOV | Op::MOVZX | Op::MOVNTI => {
                let v = self.read(m, b.ok_or_else(bad)?)?;
                self.write(m, a.ok_or_else(bad)?, v)?;
            }
            Op::MOVSX | Op::MOVSXD => {
                let src = b.ok_or_else(bad)?;
                let v = signed(self.read(m, src)?, src.width()) as u64;
                self.write(m, a.ok_or_else(bad)?, v)?;
            }
            Op::LEA => {
                let Some(Value::Memory { address, .. }) = b else {
                    return Err(bad());
                };
                self.write(m, a.ok_or_else(bad)?, address)?;
            }
            Op::ADD
            | Op::ADC
            | Op::SUB
            | Op::SBB
            | Op::CMP
            | Op::AND
            | Op::OR
            | Op::XOR
            | Op::TEST => {
                let dst = a.ok_or_else(bad)?;
                let left = self.read(m, dst)?;
                let right = self.read(m, b.ok_or_else(bad)?)?;
                let carry = self.flags & 1 != 0;
                let v = match op {
                    Op::ADD | Op::ADC => {
                        self.arithmetic(left, right, dst.width(), false, op == Op::ADC && carry)
                    }
                    Op::SUB | Op::SBB | Op::CMP => {
                        self.arithmetic(left, right, dst.width(), true, op == Op::SBB && carry)
                    }
                    Op::AND | Op::TEST => self.logical(left & right, dst.width()),
                    Op::OR => self.logical(left | right, dst.width()),
                    _ => self.logical(left ^ right, dst.width()),
                };
                if !matches!(op, Op::CMP | Op::TEST) {
                    self.write(m, dst, v)?;
                }
            }
            Op::INC | Op::DEC | Op::NEG | Op::NOT => {
                let dst = a.ok_or_else(bad)?;
                let old = self.read(m, dst)?;
                let cf = self.flags & 1;
                let v = match op {
                    Op::NOT => !old,
                    Op::NEG => self.arithmetic(0, old, dst.width(), true, false),
                    _ => self.arithmetic(old, 1, dst.width(), op == Op::DEC, false),
                };
                if matches!(op, Op::INC | Op::DEC) {
                    self.flags = (self.flags & !1) | cf;
                }
                self.write(m, dst, v)?;
            }
            Op::SHLD | Op::SHRD => {
                let dst = a.ok_or_else(bad)?;
                let bits = dst.width() * 8;
                let old = self.read(m, dst)?;
                let src = self.read(m, b.ok_or_else(bad)?)?;
                let count =
                    (self.read(m, c.ok_or_else(bad)?)? & if bits == 64 { 63 } else { 31 }) as usize;
                if count > bits {
                    return Err(bad());
                } // Architecturally undefined for 16-bit counts >16.
                if count != 0 {
                    let combined = if op == Op::SHLD {
                        ((old as u128) << bits) | src as u128
                    } else {
                        ((src as u128) << bits) | old as u128
                    };
                    let (value, carry) = if op == Op::SHLD {
                        (
                            ((combined << count) >> bits) as u64,
                            (old >> (bits - count)) & 1,
                        )
                    } else {
                        ((combined >> count) as u64, (old >> (count - 1)) & 1)
                    };
                    let value = self.logical(value, dst.width());
                    self.flags |= carry;
                    if count == 1 {
                        self.flags |= (((old ^ value) >> (bits - 1)) & 1) << 11;
                    }
                    self.write(m, dst, value)?;
                }
            }
            Op::SHL | Op::SAL | Op::SHR | Op::SAR => {
                let dst = a.ok_or_else(bad)?;
                let old = self.read(m, dst)?;
                let bits = dst.width() * 8;
                let n =
                    (self.read(m, b.ok_or_else(bad)?)? & if bits == 64 { 63 } else { 31 }) as usize;
                if n != 0 {
                    let v = match op {
                        Op::SHR => old >> n,
                        Op::SAR => (signed(old, dst.width()) >> n) as u64,
                        _ => old << n,
                    };
                    let cf = if n > bits {
                        false
                    } else if matches!(op, Op::SHL | Op::SAL) {
                        old & (1 << (bits - n)) != 0
                    } else {
                        old & (1 << (n - 1)) != 0
                    };
                    let v = self.logical(v, dst.width());
                    self.flags |= cf as u64;
                    if n == 1 {
                        let of = match op {
                            Op::SHR => old >> (bits - 1) != 0,
                            Op::SAR => false,
                            _ => (v >> (bits - 1) != 0) ^ cf,
                        };
                        self.flags |= (of as u64) << 11;
                    }
                    self.write(m, dst, v)?;
                }
            }
            Op::IMUL if b.is_some() => {
                let dst = a.ok_or_else(bad)?;
                let (left, right) = if let Some(c) = c {
                    (b.unwrap(), c)
                } else {
                    (dst, b.unwrap())
                };
                let w = dst.width();
                let product = signed(self.read(m, left)?, w) * signed(self.read(m, right)?, w);
                let truncated = product as u64 & mask(w);
                let overflow = signed(truncated, w) != product;
                self.flags = (self.flags & !0x801) | overflow as u64 | ((overflow as u64) << 11);
                self.write(m, dst, truncated)?;
            }
            Op::MUL | Op::IMUL | Op::DIV | Op::IDIV => {
                let src = a.ok_or_else(bad)?;
                let w = src.width();
                let bits = w * 8;
                let divisor = self.read(m, src)? & mask(w);
                let low = self.regs[0] & mask(w);
                if matches!(op, Op::MUL | Op::IMUL) {
                    let product = if op == Op::MUL {
                        low as u128 * divisor as u128
                    } else {
                        (signed(low, w) * signed(divisor, w)) as u128
                    };
                    let overflow = if op == Op::MUL {
                        product >> bits != 0
                    } else {
                        signed(product as u64, w) != product as i128
                    };
                    self.flags =
                        (self.flags & !0x801) | overflow as u64 | ((overflow as u64) << 11);
                    if w == 1 {
                        self.write(m, Self::reg(0, 2), product as u64)?;
                    } else {
                        self.write(m, Self::reg(0, w), product as u64)?;
                        self.write(m, Self::reg(2, w), (product >> bits) as u64)?;
                    }
                } else {
                    let dividend = if w == 1 {
                        (self.regs[0] & 65535) as u128
                    } else {
                        ((self.regs[2] & mask(w)) as u128) << bits | low as u128
                    };
                    let (q, r) = if op == Op::DIV {
                        let q = dividend
                            .checked_div(divisor as u128)
                            .ok_or(Error::ArithmeticFault)?;
                        if q > mask(w) as u128 {
                            return Err(Error::ArithmeticFault);
                        }
                        (q as u64, (dividend % divisor as u128) as u64)
                    } else {
                        let value = if w == 8 {
                            dividend as i128
                        } else {
                            ((dividend << (128 - 2 * bits)) as i128) >> (128 - 2 * bits)
                        };
                        let d = signed(divisor, w);
                        let q = value.checked_div(d).ok_or(Error::ArithmeticFault)?;
                        if q < -(1i128 << (bits - 1)) || q >= (1i128 << (bits - 1)) {
                            return Err(Error::ArithmeticFault);
                        }
                        (q as u64, (value % d) as u64)
                    };
                    if w == 1 {
                        self.write(m, Self::reg(0, 2), (q & 255) | ((r & 255) << 8))?;
                    } else {
                        self.write(m, Self::reg(0, w), q)?;
                        self.write(m, Self::reg(2, w), r)?;
                    }
                }
            }
            Op::ROL | Op::ROR => {
                let dst = a.ok_or_else(bad)?;
                let w = dst.width();
                let bits = w * 8;
                let v = self.read(m, dst)?;
                let count =
                    self.read(m, b.ok_or_else(bad)?)? as usize & if bits == 64 { 63 } else { 31 };
                if count != 0 {
                    let n = count % bits;
                    let v = if n == 0 {
                        v
                    } else if op == Op::ROL {
                        (v << n) | (v >> (bits - n))
                    } else {
                        (v >> n) | (v << (bits - n))
                    } & mask(w);
                    let cf = if op == Op::ROL {
                        v & 1
                    } else {
                        v >> (bits - 1)
                    };
                    self.flags = (self.flags & !1) | cf;
                    if count == 1 {
                        let of = if op == Op::ROL {
                            (v >> (bits - 1)) ^ cf
                        } else {
                            (v >> (bits - 1)) ^ (v >> (bits - 2))
                        };
                        self.flags = (self.flags & !2048) | ((of & 1) << 11);
                    }
                    self.write(m, dst, v)?;
                }
            }
            Op::BSWAP => {
                let dst = a.ok_or_else(bad)?;
                let v = self.read(m, dst)?;
                self.write(
                    m,
                    dst,
                    if dst.width() == 4 {
                        (v as u32).swap_bytes() as u64
                    } else {
                        v.swap_bytes()
                    },
                )?;
            }
            Op::XCHG => {
                let a = a.ok_or_else(bad)?;
                let b = b.ok_or_else(bad)?;
                let av = self.read(m, a)?;
                let bv = self.read(m, b)?;
                self.write(m, a, bv)?;
                self.write(m, b, av)?;
            }
            Op::CMPXCHG => {
                let dst = a.ok_or_else(bad)?;
                let src = b.ok_or_else(bad)?;
                if let Value::Memory { address, width } = dst {
                    m.check(address, width, blink_core::Access::Write)?;
                }
                let accumulator = Self::reg(0, dst.width());
                let old = self.read(m, dst)?;
                let v = self.read(m, src)?;
                let acc = self.read(m, accumulator)?;
                self.arithmetic(acc, old, dst.width(), true, false);
                if acc == old {
                    self.write(m, dst, v)?;
                } else {
                    self.write(m, accumulator, old)?;
                }
            }
            Op::XADD => {
                let dst = a.ok_or_else(bad)?;
                let src = b.ok_or_else(bad)?;
                let old = self.read(m, dst)?;
                let v = self.read(m, src)?;
                let sum = self.arithmetic(old, v, dst.width(), false, false);
                self.write(m, src, old)?;
                self.write(m, dst, sum)?;
            }
            Op::PUSH | Op::PUSHF => {
                let w = if ins.prefixes.operand_size() { 2 } else { 8 };
                let v = if op == Op::PUSHF {
                    self.flags
                } else {
                    self.read(m, a.ok_or_else(bad)?)?
                };
                let sp = self.regs[4].wrapping_sub(w as u64);
                m.store(sp, v, w)?;
                self.regs[4] = sp;
            }
            Op::POPF => {
                let width = if ins.prefixes.operand_size() { 2 } else { 8 };
                let value = m.load(self.regs[4], width)?;
                // Single-step and alignment-check traps are not implemented yet.
                if value & 0x40100 != 0 {
                    return Err(bad());
                }
                let allowed = 0x204cd5 & mask(width);
                self.flags = (self.flags & !allowed) | (value & allowed) | 2;
                self.regs[4] = self.regs[4].wrapping_add(width as u64);
            }
            Op::POP => {
                let w = if ins.prefixes.operand_size() { 2 } else { 8 };
                let v = m.load(self.regs[4], w)?;
                self.regs[4] = self.regs[4].wrapping_add(w as u64);
                let dst = decode::operand(self, &ins, 0, next, false)?;
                self.write(m, dst, v)?;
            }
            Op::CALL | Op::JMP => {
                let dst = a.ok_or_else(bad)?;
                let v = self.read(m, dst)?;
                let target = if matches!(dst, Value::Immediate(_)) {
                    next.wrapping_add(v)
                } else {
                    v
                };
                if op == Op::CALL {
                    let sp = self.regs[4].wrapping_sub(8);
                    m.store(sp, next, 8)?;
                    self.regs[4] = sp;
                }
                self.rip = target;
            }
            Op::RETURN => {
                self.rip = m.load(self.regs[4], 8)?;
                let extra = if let Some(a) = a { self.read(m, a)? } else { 0 };
                self.regs[4] = self.regs[4].wrapping_add(8).wrapping_add(extra);
            }
            Op::LEAVE => {
                let w = if ins.prefixes.operand_size() { 2 } else { 8 };
                let v = m.load(self.regs[5], w)?;
                self.regs[4] = self.regs[5].wrapping_add(w as u64);
                self.write(m, Self::reg(5, w), v)?;
            }
            Op::CBW => {
                let v = signed(self.regs[0], 1) as u64;
                self.write(m, Self::reg(0, 2), v)?;
            }
            Op::CWDE => {
                let v = signed(self.regs[0], 2) as u64;
                self.write(m, Self::reg(0, 4), v)?;
            }
            Op::CDQE => {
                self.regs[0] = self.regs[0] as u32 as i32 as i64 as u64;
            }
            Op::CWD | Op::CDQ | Op::CQO => {
                let w = match op {
                    Op::CWD => 2,
                    Op::CDQ => 4,
                    _ => 8,
                };
                let v = if signed(self.regs[0], w) < 0 {
                    u64::MAX
                } else {
                    0
                };
                self.write(m, Self::reg(2, w), v)?;
            }
            Op::BT | Op::BTS | Op::BTR | Op::BTC => {
                let mut dst = a.ok_or_else(bad)?;
                let src = b.ok_or_else(bad)?;
                let index = self.read(m, src)?;
                let bits = (dst.width() * 8) as u64;
                if let Value::Memory { address, width } = dst {
                    if matches!(src, Value::Register { .. }) {
                        let signed_index = signed(index, src.width()) as i64;
                        let offset = signed_index
                            .div_euclid(bits as i64)
                            .wrapping_mul(width as i64);
                        dst = Value::Memory {
                            address: address.wrapping_add(offset as u64),
                            width,
                        };
                    }
                }
                let bit = index & (bits - 1);
                let value = self.read(m, dst)?;
                self.flags = (self.flags & !1) | ((value >> bit) & 1);
                if op != Op::BT {
                    let mask = 1u64 << bit;
                    let value = match op {
                        Op::BTS => value | mask,
                        Op::BTR => value & !mask,
                        _ => value ^ mask,
                    };
                    self.write(m, dst, value)?;
                }
            }
            Op::TZCNT | Op::LZCNT => {
                let dst = a.ok_or_else(bad)?;
                let bits = (dst.width() * 8) as u32;
                let value = self.read(m, b.ok_or_else(bad)?)? & mask(dst.width());
                let count = if op == Op::TZCNT {
                    value.trailing_zeros().min(bits)
                } else {
                    value.leading_zeros() - (64 - bits)
                };
                self.write(m, dst, count as u64)?;
                self.flags =
                    (self.flags & !65) | u64::from(value == 0) | (u64::from(count == 0) << 6);
            }
            Op::BSF | Op::BSR => {
                let value = self.read(m, b.ok_or_else(bad)?)?;
                self.flags = (self.flags & !64) | ((value == 0) as u64) << 6;
                if value != 0 {
                    let result = if op == Op::BSF {
                        value.trailing_zeros()
                    } else {
                        63 - value.leading_zeros()
                    };
                    self.write(m, a.ok_or_else(bad)?, result as u64)?;
                }
            }
            Op::RDTSC | Op::RDTSCP => {
                self.regs[0] = self.ticks as u32 as u64;
                self.regs[2] = self.ticks >> 32;
                if op == Op::RDTSCP {
                    self.regs[1] = 0;
                }
            }
            Op::CPUID => {
                let leaf = self.regs[0] as u32;
                self.regs[0] = 0;
                self.regs[1] = 0;
                self.regs[2] = 0;
                self.regs[3] = 0;
                if leaf == 0 {
                    self.regs[0] = 1;
                    self.regs[3] = u32::from_le_bytes(*b"Blin") as u64;
                    self.regs[2] = u32::from_le_bytes(*b"kRus") as u64;
                    self.regs[1] = u32::from_le_bytes(*b"tCPU") as u64;
                }
            }
            Op::LAHF => self.regs[0] = (self.regs[0] & !0xff00) | ((self.flags & 0xd5 | 2) << 8),
            Op::SAHF => self.flags = (self.flags & !0xd5) | ((self.regs[0] >> 8) & 0xd5) | 2,
            Op::LOOP | Op::LOOPZ | Op::LOOPNZ => {
                let w = if ins.prefixes.address_size() { 4 } else { 8 };
                let counter = self.regs[1].wrapping_sub(1) & mask(w);
                self.regs[1] = counter;
                let z = self.flags & 64 != 0;
                if counter != 0
                    && (op == Op::LOOP || (op == Op::LOOPZ && z) || (op == Op::LOOPNZ && !z))
                {
                    self.rip = next.wrapping_add(self.read(m, a.ok_or_else(bad)?)?);
                }
            }
            Op::JRCXZ | Op::JECXZ => {
                let zero = if op == Op::JECXZ {
                    self.regs[1] as u32 == 0
                } else {
                    self.regs[1] == 0
                };
                if zero {
                    self.rip = next.wrapping_add(self.read(m, a.ok_or_else(bad)?)?);
                }
            }
            Op::CLC => self.flags &= !1,
            Op::STC => self.flags |= 1,
            Op::CMC => self.flags ^= 1,
            Op::CLD => self.flags &= !1024,
            Op::STD => self.flags |= 1024,
            Op::JO
            | Op::JNO
            | Op::JB
            | Op::JNB
            | Op::JZ
            | Op::JNZ
            | Op::JA
            | Op::JNA
            | Op::JS
            | Op::JNS
            | Op::JP
            | Op::JNP
            | Op::JL
            | Op::JGE
            | Op::JLE
            | Op::JG => {
                if self.condition(op.condition().unwrap()) {
                    self.rip = next.wrapping_add(self.read(m, a.ok_or_else(bad)?)?);
                }
            }
            Op::CMOVA
            | Op::CMOVB
            | Op::CMOVG
            | Op::CMOVGE
            | Op::CMOVL
            | Op::CMOVLE
            | Op::CMOVNA
            | Op::CMOVNB
            | Op::CMOVNO
            | Op::CMOVNP
            | Op::CMOVNS
            | Op::CMOVNZ
            | Op::CMOVO
            | Op::CMOVP
            | Op::CMOVS
            | Op::CMOVZ => {
                let v = self.read(m, b.ok_or_else(bad)?)?;
                let dst = a.ok_or_else(bad)?;
                if self.condition(op.condition().unwrap()) {
                    self.write(m, dst, v)?;
                } else if dst.width() == 4 {
                    let old = self.read(m, dst)?;
                    self.write(m, dst, old)?;
                }
            }
            Op::SETO
            | Op::SETNO
            | Op::SETB
            | Op::SETAE
            | Op::SETZ
            | Op::SETNZ
            | Op::SETBE
            | Op::SETA
            | Op::SETS
            | Op::SETNS
            | Op::SETP
            | Op::SETNP
            | Op::SETL
            | Op::SETGE
            | Op::SETLE
            | Op::SETG => {
                let v = self.condition(op.condition().unwrap()) as u64;
                self.write(m, a.ok_or_else(bad)?, v)?;
            }
            _ => return Err(bad()),
        }
        if mmx {
            self.fpu.occupied = [true; 8];
            self.fpu.top = 0;
        }
        Ok(Step::Continue)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use blink_core::Permissions;
    #[test]
    fn page_end() {
        let mut m = Memory::new(4096);
        m.map(0x1000, 4096, Permissions::RX).unwrap();
        m.initialize(0x1ffe, &[0x0f, 0x05]).unwrap();
        assert!(matches!(
            X86::new(0x1ffe, 0).step(&mut m).unwrap(),
            Step::Syscall { .. }
        ));
    }
    #[test]
    fn flags() {
        let mut c = X86::new(0, 0);
        assert_eq!(c.arithmetic(u64::MAX, 1, 8, false, false), 0);
        assert_eq!(c.flags & 65, 65);
        c.arithmetic(i64::MAX as u64, 1, 8, false, false);
        assert_ne!(c.flags & 2048, 0);
        assert_eq!(c.flags & 1, 0);
    }
    #[test]
    fn carry_input_wraps_operand_without_losing_carry() {
        let mut c = X86::new(0, 0);
        assert_eq!(c.arithmetic(0, u64::MAX, 8, false, true), 0);
        assert_eq!(c.flags & 1, 1);
        assert_eq!(c.arithmetic(0, u64::MAX, 8, true, true), 0);
        assert_eq!(c.flags & 1, 1);
    }
    #[test]
    fn partial_register_writes() {
        let mut m = Memory::new(4096);
        m.map(0x1000, 4096, Permissions::RX).unwrap();
        m.initialize(
            0x1000,
            &[0xb4, 0x42, 0x66, 0xb8, 0x34, 0x12, 0xb8, 7, 0, 0, 0],
        )
        .unwrap();
        let mut c = X86::new(0x1000, 0);
        c.regs[0] = u64::MAX;
        c.step(&mut m).unwrap();
        assert_eq!(c.regs[0], 0xffffffffffff42ff);
        c.step(&mut m).unwrap();
        assert_eq!(c.regs[0], 0xffffffffffff1234);
        c.step(&mut m).unwrap();
        assert_eq!(c.regs[0], 7);
    }
}

#[cfg(test)]
mod adapter_regressions {
    use super::*;
    use blink_core::Permissions;
    #[test]
    fn movsxd_without_rex_w_zero_extends_destination() {
        let mut m = Memory::new(4096);
        m.map(0x1000, 4096, Permissions::RX).unwrap();
        m.initialize(0x1000, &[0x63, 0xc1]).unwrap();
        let mut c = X86::new(0x1000, 0);
        c.regs[0] = u64::MAX;
        c.regs[1] = 0x80000000;
        c.step(&mut m).unwrap();
        assert_eq!(c.regs[0], 0x80000000);
    }
    #[test]
    fn arithmetic_preserves_direction_flag() {
        let mut c = X86::new(0, 0);
        c.flags |= 1024;
        c.arithmetic(1, 2, 8, false, false);
        assert_ne!(c.flags & 1024, 0);
        c.logical(0, 8);
        assert_ne!(c.flags & 1024, 0);
    }
    #[test]
    fn cmpxchg_failure_still_requires_writable_memory() {
        let mut m = Memory::new(4096);
        m.map(0x1000, 4096, Permissions::RX).unwrap();
        m.initialize(0x1000, &[0x48, 0x0f, 0xb1, 0x1f]).unwrap();
        let mut c = X86::new(0x1000, 0);
        c.regs[7] = 0x1800;
        c.regs[0] = 1;
        assert!(matches!(
            c.step(&mut m),
            Err(Error::MemoryFault {
                access: blink_core::Access::Write,
                ..
            })
        ));
    }
}

#[cfg(test)]
mod vector_tests {
    use super::*;
    use blink_core::Permissions;
    #[test]
    fn scalar_move_and_signal_preserve_upper_lanes() {
        let mut m = Memory::new(8192);
        m.map(0x1000, 4096, Permissions::RX).unwrap();
        m.map(0x2000, 4096, Permissions::RW).unwrap();
        m.initialize(0x1000, &[0xf2, 0x0f, 0x10, 0xc1]).unwrap();
        let mut c = X86::new(0x1000, 0x3000);
        c.xmm[0] = [0xaa; 16];
        c.xmm[1] = [0xbb; 16];
        c.step(&mut m).unwrap();
        assert_eq!(c.xmm[0][..8], [0xbb; 8]);
        assert_eq!(c.xmm[0][8..], [0xaa; 8]);
        let before = c.xmm;
        c.mxcsr = 0x5fa0;
        c.enter_signal(&mut m, 10, 0x1000, 0x1010, 0, 1).unwrap();
        assert_eq!(c.mxcsr, 0x1f80);
        c.mxcsr = 0x3f80;
        c.xmm = [[0; 16]; 16];
        c.regs[4] += 8;
        c.restore_signal(&m).unwrap();
        assert_eq!(c.xmm, before);
        assert_eq!(c.mxcsr, 0x5fa0);
    }
    #[test]
    fn fs_negative_displacement_is_sign_extended() {
        let mut m = Memory::new(8192);
        m.map(0x1000, 4096, Permissions::RX).unwrap();
        m.map(0x2000, 4096, Permissions::RW).unwrap();
        m.initialize(
            0x1000,
            &[0x64, 0x48, 0x8b, 0x04, 0x25, 0xd8, 0xff, 0xff, 0xff],
        )
        .unwrap();
        m.store(0x2000, 42, 8).unwrap();
        let mut c = X86::new(0x1000, 0);
        c.fs_base = 0x2028;
        c.step(&mut m).unwrap();
        assert_eq!(c.regs[0], 42);
    }
}

#[cfg(test)]
mod byte_swap_tests {
    use super::*;
    #[test]
    fn prefetch_invalid_address_is_only_a_hint() {
        let mut memory = Memory::new(4096);
        memory.map(4096, 4096, blink_core::Permissions::RX).unwrap();
        memory.initialize(4096, &[0x0f, 0x18, 0x0a]).unwrap();
        let mut cpu = X86::new(4096, 0);
        cpu.regs[2] = u64::MAX;
        cpu.flags = 0x8d7;
        cpu.step(&mut memory).unwrap();
        assert_eq!(cpu.rip, 4099);
        assert_eq!(cpu.flags, 0x8d7);
    }
    #[test]
    fn byte_swap_register_width() {
        let mut memory = Memory::new(4096);
        memory.map(4096, 4096, blink_core::Permissions::RX).unwrap();
        memory
            .initialize(4096, &[0x0f, 0xce, 0x48, 0x0f, 0xca])
            .unwrap();
        let mut cpu = X86::new(4096, 0);
        cpu.regs[6] = 0x1122334412345678;
        cpu.regs[2] = 0x1122334455667788;
        cpu.step(&mut memory).unwrap();
        assert_eq!(cpu.regs[6], 0x78563412);
        cpu.step(&mut memory).unwrap();
        assert_eq!(cpu.regs[2], 0x8877665544332211);
    }
}

#[cfg(test)]
mod sse_memory_tests {
    use super::*;
    use blink_core::{Access, Permissions};
    fn run(code: &[u8], address: u64) -> (X86, Memory) {
        let mut memory = Memory::new(8192);
        memory.map(4096, 4096, Permissions::RX).unwrap();
        memory.map(8192, 4096, Permissions::RW).unwrap();
        memory.initialize(4096, code).unwrap();
        let mut cpu = X86::new(4096, 0);
        cpu.regs[0] = address;
        cpu.xmm[0] = [0x5a; 16];
        (cpu, memory)
    }
    #[test]
    fn mmx_conversion_reads_eight_bytes_and_faults_preserve_tags() {
        for opcode in [0x2c, 0x2d] {
            let (mut cpu, mut memory) = run(&[0x0f, opcode, 0x00], 12280);
            memory.store(12280, 0x400000003f800000, 8).unwrap();
            cpu.step(&mut memory).unwrap();
            assert_eq!(cpu.fpu.values[0], (0xffffu128 << 64) | 0x200000001);
            assert_eq!(cpu.fpu.occupied, [true; 8]);
            assert_eq!(cpu.fpu.top, 0);
        }
        let (mut cpu, mut memory) = run(&[0x0f, 0x6f, 0x00], 12284);
        cpu.fpu.top = 3;
        assert!(matches!(
            cpu.step(&mut memory),
            Err(Error::MemoryFault { .. })
        ));
        assert_eq!(cpu.fpu.top, 3);
        assert_eq!(cpu.fpu.occupied, [false; 8]);
        let (mut cpu, mut memory) = run(&[0x0f, 0x6f, 0x00], 8192);
        cpu.fpu.control &= !4;
        cpu.fpu.status |= 4;
        assert!(matches!(cpu.step(&mut memory), Err(Error::ArithmeticFault)));
        assert_eq!(cpu.fpu.occupied, [false; 8]);
    }
    #[test]
    fn masked_store_checks_selected_bytes_and_segment_address() {
        let code = [0x64, 0x66, 0x0f, 0xf7, 0xc1];
        let (mut cpu, mut memory) = run(&code, 0);
        cpu.regs[7] = 8192;
        cpu.fs_base = 4095;
        cpu.xmm[1][0] = 128;
        cpu.step(&mut memory).unwrap();
        assert_eq!(memory.load(12287, 1).unwrap(), 0x5a);
        memory.store(12287, 0, 1).unwrap();
        cpu.rip = 4096;
        cpu.xmm[1][1] = 128;
        assert!(matches!(
            cpu.step(&mut memory),
            Err(Error::MemoryFault { address: 12288, .. })
        ));
        assert_eq!(memory.load(12287, 1).unwrap(), 0);
        let (mut cpu, mut memory) = run(&[0x67, 0x66, 0x0f, 0xf7, 0xc1], 0);
        cpu.regs[7] = 0xffff000000002000;
        cpu.xmm[1][0] = 128;
        cpu.step(&mut memory).unwrap();
        assert_eq!(memory.load(8192, 1).unwrap(), 0x5a);
    }
    #[test]
    fn flush_checks_one_byte_and_pause_preserves_flags() {
        let (mut cpu, mut memory) = run(&[0x0f, 0xae, 0x38, 0xf3, 0x90], 12287);
        cpu.flags = 0x8d7;
        cpu.step(&mut memory).unwrap();
        cpu.step(&mut memory).unwrap();
        assert_eq!(cpu.flags, 0x8d7);
        cpu.rip = 4096;
        cpu.regs[0] = 12288;
        assert!(matches!(
            cpu.step(&mut memory),
            Err(Error::MemoryFault { address: 12288, .. })
        ));
    }
    #[test]
    fn insert_word_reads_only_two_bytes_at_page_end() {
        let (mut cpu, mut memory) = run(&[0x66, 0x0f, 0xc4, 0x00, 7], 12286);
        memory.store(12286, 0x1234, 2).unwrap();
        cpu.step(&mut memory).unwrap();
        assert_eq!(&cpu.xmm[0][..14], &[0x5a; 14]);
        assert_eq!(&cpu.xmm[0][14..], &[0x34, 0x12]);
    }
    #[test]
    fn legacy_packed_memory_alignment_and_atomic_faults() {
        // PADDB and MOVNTDQ require alignment, while MOVDQU accepts the same pointer.
        for code in [&[0x66, 0x0f, 0xfc, 0x00][..], &[0x66, 0x0f, 0xe7, 0x00][..]] {
            let (mut cpu, mut memory) = run(code, 8193);
            assert!(matches!(
                cpu.step(&mut memory),
                Err(Error::MemoryFault { address: 8193, .. })
            ));
            assert_eq!(cpu.xmm[0], [0x5a; 16]);
            assert_eq!(memory.load(8193, 8).unwrap(), 0);
        }
        let (mut cpu, mut memory) = run(&[0xf3, 0x0f, 0x6f, 0x00], 8193);
        memory.write(8193, &[0x12; 16]).unwrap();
        cpu.step(&mut memory).unwrap();
        assert_eq!(cpu.xmm[0], [0x12; 16]);
        let (mut cpu, mut memory) = run(&[0x66, 0x0f, 0xfc, 0x00], 12288);
        assert!(matches!(
            cpu.step(&mut memory),
            Err(Error::MemoryFault {
                access: Access::Read,
                ..
            })
        ));
        assert_eq!(cpu.xmm[0], [0x5a; 16]);
    }
}
