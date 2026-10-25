//! AMD64 integer interpreter with a bounded decoder adapter.
mod decode;
mod signal;
use blink_core::{Cpu, Error, Memory, Result, Step};
use decode::Value;
use yaxpeax_x86::long_mode::{ConditionCode as C, Opcode as Op};
#[derive(Clone)]
pub struct X86 {
    pub regs: [u64; 16],
    pub rip: u64,
    pub flags: u64,
    pub fs_base: u64,
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
        };
        c.regs[4] = stack;
        c
    }
    fn read(&self, m: &Memory, v: Value) -> Result<u64> {
        Ok(match v {
            Value::Immediate(v) => v,
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
        self.rip = next;
        match op {
            Op::NOP | Op::ENDBR64 => {}
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
            Op::MOV | Op::MOVZX => {
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
