//! AMD64 bootstrap interpreter. Fetches only bytes actually consumed.
use blink_core::{Cpu, Error, Memory, Result, Step};
#[derive(Clone)]
pub struct X86 {
    pub regs: [u64; 16],
    pub rip: u64,
    pub flags: u64,
}
#[derive(Clone, Copy)]
enum Operand {
    Reg(usize),
    Mem(u64),
}
fn mask(w: usize) -> u64 {
    if w == 8 {
        u64::MAX
    } else {
        (1u64 << (w * 8)) - 1
    }
}
impl X86 {
    pub fn new(entry: u64, stack: u64) -> Self {
        let mut s = Self {
            regs: [0; 16],
            rip: entry,
            flags: 2,
        };
        s.regs[4] = stack;
        s
    }
    fn byte(&mut self, m: &Memory) -> Result<u8> {
        let b = m.fetch(self.rip)?;
        self.rip = self.rip.wrapping_add(1);
        Ok(b)
    }
    fn imm(&mut self, m: &Memory, n: usize) -> Result<u64> {
        let mut v = 0;
        for i in 0..n {
            v |= (self.byte(m)? as u64) << (i * 8);
        }
        Ok(v)
    }
    fn rm(&mut self, m: &Memory, rex: u8, extra: usize) -> Result<(usize, Operand)> {
        let b = self.byte(m)?;
        let mode = b >> 6;
        let reg = ((b >> 3) & 7) as usize + if rex & 4 != 0 { 8 } else { 0 };
        let low = b & 7;
        let r = low as usize + if rex & 1 != 0 { 8 } else { 0 };
        if mode == 3 {
            return Ok((reg, Operand::Reg(r)));
        }
        let mut addr;
        if low == 4 {
            let sib = self.byte(m)?;
            let base = sib & 7;
            let index = (sib >> 3) & 7;
            addr = if mode == 0 && base == 5 {
                self.imm(m, 4)? as u32 as i32 as i64 as u64
            } else {
                self.regs[base as usize + if rex & 1 != 0 { 8 } else { 0 }]
            };
            if index != 4 || rex & 2 != 0 {
                addr = addr.wrapping_add(
                    self.regs[index as usize + if rex & 2 != 0 { 8 } else { 0 }] << (sib >> 6),
                );
            }
        } else if mode == 0 && low == 5 {
            let d = self.imm(m, 4)? as u32 as i32 as i64 as u64;
            return Ok((
                reg,
                Operand::Mem(self.rip.wrapping_add(extra as u64).wrapping_add(d)),
            ));
        } else {
            addr = self.regs[r];
        }
        let d = match mode {
            1 => self.imm(m, 1)? as u8 as i8 as i64 as u64,
            2 => self.imm(m, 4)? as u32 as i32 as i64 as u64,
            _ => 0,
        };
        Ok((reg, Operand::Mem(addr.wrapping_add(d))))
    }
    fn read(&self, m: &Memory, o: Operand, w: usize) -> Result<u64> {
        Ok(match o {
            Operand::Reg(r) => self.regs[r] & mask(w),
            Operand::Mem(a) => m.load(a, w)?,
        })
    }
    fn write(&mut self, m: &mut Memory, o: Operand, w: usize, v: u64) -> Result<()> {
        match o {
            Operand::Reg(r) => {
                self.regs[r] = if w >= 4 {
                    v & mask(w)
                } else {
                    (self.regs[r] & !mask(w)) | (v & mask(w))
                };
                Ok(())
            }
            Operand::Mem(a) => m.store(a, v, w),
        }
    }
    fn arithmetic(&mut self, a: u64, b: u64, w: usize, sub: bool) -> u64 {
        let mask = mask(w);
        let sign = 1u64 << (w * 8 - 1);
        let a = a & mask;
        let b = b & mask;
        let v = if sub {
            a.wrapping_sub(b)
        } else {
            a.wrapping_add(b)
        } & mask;
        let carry = if sub {
            a < b
        } else {
            a as u128 + b as u128 > mask as u128
        };
        let overflow = if sub {
            (a ^ b) & (a ^ v) & sign != 0
        } else {
            !(a ^ b) & (a ^ v) & sign != 0
        };
        self.flags = 2
            | carry as u64
            | (((v as u8).count_ones() % 2 == 0) as u64) << 2
            | (((a ^ b ^ v) & 16 != 0) as u64) << 4
            | ((v == 0) as u64) << 6
            | ((v & sign != 0) as u64) << 7
            | (overflow as u64) << 11;
        v
    }
    fn logical(&mut self, v: u64, w: usize) -> u64 {
        let v = v & mask(w);
        self.flags = 2
            | (((v as u8).count_ones() % 2 == 0) as u64) << 2
            | ((v == 0) as u64) << 6
            | ((v & (1 << (w * 8 - 1)) != 0) as u64) << 7;
        v
    }
    fn condition(&self, c: u8) -> bool {
        let f = self.flags;
        let o = f & 2048 != 0;
        let s = f & 128 != 0;
        let z = f & 64 != 0;
        let p = f & 4 != 0;
        let b = f & 1 != 0;
        match c {
            0 => o,
            1 => !o,
            2 => b,
            3 => !b,
            4 => z,
            5 => !z,
            6 => b || z,
            7 => !b && !z,
            8 => s,
            9 => !s,
            10 => p,
            11 => !p,
            12 => s != o,
            13 => s == o,
            14 => z || s != o,
            _ => !z && s == o,
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
        let mut op = self.byte(m)?;
        let mut rex = 0;
        if (0x40..=0x4f).contains(&op) {
            rex = op;
            op = self.byte(m)?;
        }
        let w = if rex & 8 != 0 { 8 } else { 4 };
        match op {
            0x90 => {}
            0xb8..=0xbf => {
                let r = (op - 0xb8) as usize + if rex & 1 != 0 { 8 } else { 0 };
                self.regs[r] = self.imm(m, w)?;
            }
            0x50..=0x57 => {
                let r = (op - 0x50) as usize + if rex & 1 != 0 { 8 } else { 0 };
                let sp = self.regs[4].wrapping_sub(8);
                m.store(sp, self.regs[r], 8)?;
                self.regs[4] = sp;
            }
            0x58..=0x5f => {
                let r = (op - 0x58) as usize + if rex & 1 != 0 { 8 } else { 0 };
                let v = m.load(self.regs[4], 8)?;
                self.regs[4] = self.regs[4].wrapping_add(8);
                self.regs[r] = v;
            }
            0xc3 => {
                self.rip = m.load(self.regs[4], 8)?;
                self.regs[4] = self.regs[4].wrapping_add(8);
            }
            0xe8 | 0xe9 | 0xeb => {
                let d = if op == 0xeb {
                    self.imm(m, 1)? as u8 as i8 as i64 as u64
                } else {
                    self.imm(m, 4)? as u32 as i32 as i64 as u64
                };
                if op == 0xe8 {
                    let sp = self.regs[4].wrapping_sub(8);
                    m.store(sp, self.rip, 8)?;
                    self.regs[4] = sp;
                }
                self.rip = self.rip.wrapping_add(d);
            }
            0x70..=0x7f => {
                let d = self.imm(m, 1)? as u8 as i8 as i64 as u64;
                if self.condition(op & 15) {
                    self.rip = self.rip.wrapping_add(d);
                }
            }
            0x0f => {
                let op2 = self.byte(m)?;
                match op2 {
                    0x05 => {
                        self.regs[1] = self.rip;
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
                    0x80..=0x8f => {
                        let d = self.imm(m, 4)? as u32 as i32 as i64 as u64;
                        if self.condition(op2 & 15) {
                            self.rip = self.rip.wrapping_add(d);
                        }
                    }
                    _ => {
                        return Err(Error::UnsupportedInstruction {
                            pc,
                            encoding: 0x0f00 | op2 as u32,
                        })
                    }
                }
            }
            0x89 | 0x8b | 0x8d | 0x31 | 0x33 | 0x01 | 0x03 | 0x29 | 0x2b | 0x39 | 0x3b | 0x85 => {
                let (reg, rm) = self.rm(m, rex, 0)?;
                let ro = Operand::Reg(reg);
                if op == 0x8d {
                    if let Operand::Mem(a) = rm {
                        self.write(m, ro, w, a)?;
                    } else {
                        return Err(Error::UnsupportedInstruction {
                            pc,
                            encoding: op as u32,
                        });
                    }
                } else if op == 0x89 {
                    let v = self.read(m, ro, w)?;
                    self.write(m, rm, w, v)?;
                } else if op == 0x8b {
                    let v = self.read(m, rm, w)?;
                    self.write(m, ro, w, v)?;
                } else {
                    let (dst, src) = if matches!(op, 0x33 | 0x03 | 0x2b | 0x3b) {
                        (ro, rm)
                    } else {
                        (rm, ro)
                    };
                    let a = self.read(m, dst, w)?;
                    let b = self.read(m, src, w)?;
                    let v = match op {
                        0x31 | 0x33 => self.logical(a ^ b, w),
                        0x85 => self.logical(a & b, w),
                        0x01 | 0x03 => self.arithmetic(a, b, w, false),
                        _ => self.arithmetic(a, b, w, true),
                    };
                    if !matches!(op, 0x39 | 0x3b | 0x85) {
                        self.write(m, dst, w, v)?;
                    }
                }
            }
            0xc7 | 0x81 | 0x83 => {
                let n = if op == 0x83 { 1 } else { 4 };
                let (ext, rm) = self.rm(m, rex, n)?;
                let raw = self.imm(m, n)?;
                let b = if n == 1 {
                    raw as u8 as i8 as i64 as u64
                } else {
                    raw as u32 as i32 as i64 as u64
                };
                let ext = ext & 7;
                if op == 0xc7 {
                    if ext != 0 {
                        return Err(Error::UnsupportedInstruction {
                            pc,
                            encoding: op as u32,
                        });
                    }
                    self.write(m, rm, w, b)?;
                } else {
                    let a = self.read(m, rm, w)?;
                    let v = match ext {
                        0 => self.arithmetic(a, b, w, false),
                        1 => self.logical(a | b, w),
                        4 => self.logical(a & b, w),
                        5 | 7 => self.arithmetic(a, b, w, true),
                        6 => self.logical(a ^ b, w),
                        _ => {
                            return Err(Error::UnsupportedInstruction {
                                pc,
                                encoding: op as u32,
                            })
                        }
                    };
                    if ext != 7 {
                        self.write(m, rm, w, v)?;
                    }
                }
            }
            _ => {
                return Err(Error::UnsupportedInstruction {
                    pc,
                    encoding: op as u32,
                })
            }
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
        assert_eq!(c.arithmetic(u64::MAX, 1, 8, false), 0);
        assert_eq!(c.flags & 65, 65);
        c.arithmetic(i64::MAX as u64, 1, 8, false);
        assert_ne!(c.flags & 2048, 0);
        assert_eq!(c.flags & 1, 0);
    }
}
