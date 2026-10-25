//! A64 bootstrap interpreter; guest ISA is independent of host ISA.
use blink_core::{Access, Cpu, Error, Memory, Result, Step};
#[derive(Clone)]
pub struct Aarch64 {
    pub regs: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub nzcv: u32,
}
fn signed(v: u64, bits: u32) -> u64 {
    ((v << (64 - bits)) as i64 >> (64 - bits)) as u64
}
impl Aarch64 {
    pub fn new(entry: u64, stack: u64) -> Self {
        Self {
            regs: [0; 31],
            sp: stack,
            pc: entry,
            nzcv: 0,
        }
    }
    fn read(&self, r: usize, sp: bool) -> u64 {
        if r == 31 {
            if sp {
                self.sp
            } else {
                0
            }
        } else {
            self.regs[r]
        }
    }
    fn write(&mut self, r: usize, v: u64, wide: bool, sp: bool) {
        let v = if wide { v } else { v & 0xffffffff };
        if r == 31 {
            if sp {
                self.sp = v;
            }
        } else {
            self.regs[r] = v;
        }
    }
}
impl Cpu for Aarch64 {
    fn pc(&self) -> u64 {
        self.pc
    }
    fn set_result(&mut self, v: u64) {
        self.regs[0] = v;
    }
    fn step(&mut self, m: &mut Memory) -> Result<Step> {
        let pc = self.pc;
        if pc & 3 != 0 {
            return Err(Error::MemoryFault {
                address: pc,
                access: Access::Execute,
            });
        }
        let mut b = [0; 4];
        m.read(pc, &mut b, Access::Execute)?;
        let i = u32::from_le_bytes(b);
        self.pc = pc.wrapping_add(4);
        let rd = (i & 31) as usize;
        let rn = ((i >> 5) & 31) as usize;
        let wide = i >> 31 != 0;
        if i == 0xd4000001 {
            return Ok(Step::Syscall {
                number: self.regs[8],
                args: [
                    self.regs[0],
                    self.regs[1],
                    self.regs[2],
                    self.regs[3],
                    self.regs[4],
                    self.regs[5],
                ],
            });
        }
        if i == 0xd503201f {
            return Ok(Step::Continue);
        }
        if i & 0x1f800000 == 0x12800000 {
            let shift = ((i >> 21) & 3) * 16;
            let imm = (((i >> 5) & 0xffff) as u64) << shift;
            let kind = (i >> 29) & 3;
            if (!wide && shift >= 32) || kind == 1 {
                return Err(Error::UnsupportedInstruction { pc, encoding: i });
            }
            let v = match kind {
                0 => !imm,
                2 => imm,
                3 => (self.read(rd, false) & !(0xffffu64 << shift)) | imm,
                _ => unreachable!(),
            };
            self.write(rd, v, wide, false);
        } else if i & 0x1f000000 == 0x11000000 {
            let imm = (((i >> 10) & 4095) as u64) << if i & (1 << 22) != 0 { 12 } else { 0 };
            let sub = i & (1 << 30) != 0;
            let flags = i & (1 << 29) != 0;
            let mask = if wide { u64::MAX } else { 0xffffffff };
            let a = self.read(rn, true) & mask;
            let v = if sub {
                a.wrapping_sub(imm)
            } else {
                a.wrapping_add(imm)
            } & mask;
            if flags {
                let sign = if wide { 1 << 63 } else { 1 << 31 };
                let carry = if sub {
                    a >= imm
                } else {
                    a as u128 + imm as u128 > mask as u128
                };
                let ov = if sub {
                    (a ^ imm) & (a ^ v) & sign != 0
                } else {
                    !(a ^ imm) & (a ^ v) & sign != 0
                };
                self.nzcv = ((v & sign != 0) as u32) << 31
                    | ((v == 0) as u32) << 30
                    | (carry as u32) << 29
                    | (ov as u32) << 28;
            }
            self.write(rd, v, wide, !flags);
        } else if i & 0x9f000000 == 0x10000000 || i & 0x9f000000 == 0x90000000 {
            let imm = signed(
                (((i >> 5) & 0x7ffff) as u64) << 2 | ((i >> 29) & 3) as u64,
                21,
            );
            let v = if i >> 31 != 0 {
                (pc & !4095).wrapping_add(imm << 12)
            } else {
                pc.wrapping_add(imm)
            };
            self.write(rd, v, true, false);
        } else if i & 0x7c000000 == 0x14000000 {
            if i >> 31 != 0 {
                self.regs[30] = self.pc;
            }
            self.pc = pc.wrapping_add(signed((i & 0x3ffffff) as u64, 26) << 2);
        } else if i & 0x7e000000 == 0x34000000 {
            let v = self.read(rd, false) & if wide { u64::MAX } else { 0xffffffff };
            if (v != 0) == (i & (1 << 24) != 0) {
                self.pc = pc.wrapping_add(signed(((i >> 5) & 0x7ffff) as u64, 19) << 2);
            }
        } else if i & 0xfffffc1f == 0xd65f0000 {
            self.pc = self.read(rn, false);
        } else if i & 0xffe0ffe0 == 0xaa0003e0 {
            self.write(rd, self.read(((i >> 16) & 31) as usize, false), true, false);
        } else if i & 0x3b000000 == 0x39000000 && i & (1 << 26) == 0 {
            let size = 1usize << ((i >> 30) & 3);
            let opc = (i >> 22) & 3;
            if opc > 1 {
                return Err(Error::UnsupportedInstruction { pc, encoding: i });
            }
            let addr = self
                .read(rn, true)
                .wrapping_add((((i >> 10) & 4095) as u64) * size as u64);
            if opc == 1 {
                let v = m.load(addr, size)?;
                self.write(rd, v, size == 8, false);
            } else {
                m.store(addr, self.read(rd, false), size)?;
            }
        } else {
            return Err(Error::UnsupportedInstruction { pc, encoding: i });
        }
        Ok(Step::Continue)
    }
}
