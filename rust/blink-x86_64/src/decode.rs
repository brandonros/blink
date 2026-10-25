//! Bounded adapter around pinned yaxpeax-x86. Dependency types stay in this crate.
use crate::X86;
use blink_core::{Error, Memory, Result};
use yaxpeax_arch::{Decoder, LengthedInstruction, U8Reader};
use yaxpeax_x86::long_mode::{
    register_class, DecodeError, InstDecoder, Instruction, Operand, RegSpec, Segment,
};
#[derive(Clone, Copy, Debug)]
pub(super) enum Value {
    Vector(usize),
    FloatStack(usize),
    Register {
        index: usize,
        width: usize,
        shift: u32,
    },
    Memory {
        address: u64,
        width: usize,
    },
    Immediate(u64),
}
impl Value {
    pub fn width(self) -> usize {
        match self {
            Self::Register { width, .. } | Self::Memory { width, .. } => width,
            Self::Immediate(_) => 8,
            Self::Vector(_) => 16,
            Self::FloatStack(_) => 10,
        }
    }
}
pub(super) fn instruction(memory: &Memory, pc: u64) -> Result<(Instruction, u64)> {
    let mut bytes = [0; 15];
    let decoder = InstDecoder::default();
    for n in 0..15 {
        bytes[n] = memory.fetch(pc.wrapping_add(n as u64))?;
        match decoder.decode(&mut U8Reader::new(&bytes[..n + 1])) {
            Ok(i) => {
                let end = pc.wrapping_add(i.len().to_const());
                return Ok((i, end));
            }
            Err(DecodeError::ExhaustedInput) => {}
            Err(_) => break,
        }
    }
    Err(Error::UnsupportedInstruction {
        pc,
        encoding: u32::from_le_bytes(bytes[..4].try_into().unwrap()),
    })
}
fn register(reg: RegSpec) -> Option<Value> {
    let class = reg.class();
    let num = reg.num() as usize;
    if class == register_class::ST && num < 8 {
        return Some(Value::FloatStack(num));
    }
    if class == register_class::X && num < 16 {
        return Some(Value::Vector(num));
    }
    if ![
        register_class::Q,
        register_class::D,
        register_class::W,
        register_class::B,
        register_class::RB,
    ]
    .contains(&class)
        || num >= 16
    {
        return None;
    }
    let (index, shift) = if class == register_class::B && num >= 4 {
        (num - 4, 8)
    } else {
        (num, 0)
    };
    Some(Value::Register {
        index,
        width: reg.width() as usize,
        shift,
    })
}
fn address_reg(cpu: &X86, r: RegSpec, next: u64) -> Option<u64> {
    if r.class() == register_class::RIP {
        return Some(next);
    }
    if r.class() == register_class::EIP {
        return Some(next as u32 as u64);
    }
    let Value::Register {
        index,
        width,
        shift: 0,
    } = register(r)?
    else {
        return None;
    };
    Some(cpu.regs[index] & crate::mask(width))
}
pub(super) fn operand(
    cpu: &X86,
    i: &Instruction,
    index: u8,
    next: u64,
    lea: bool,
) -> Result<Value> {
    let bad = || Error::UnsupportedInstruction {
        pc: cpu.rip,
        encoding: i.opcode() as u32,
    };
    let op = i.operand(index);
    let addr = match op {
        Operand::Register { reg } => {
            let mut value = register(reg).ok_or_else(bad)?;
            // yaxpeax 2.2.0 unconditionally reports a qword destination for 0x63.
            // Native Blink movsxd.S proves the no-REX.W forms retain operand size.
            if i.opcode() == yaxpeax_x86::long_mode::Opcode::MOVSXD
                && !i.prefixes.rex().is_some_and(|r| r.w())
            {
                if let Value::Register { width, .. } = &mut value {
                    *width = if i.prefixes.operand_size() { 2 } else { 4 };
                }
            }
            // Like MOVSXD, yaxpeax 2.2.0 reports qword BSWAP without REX.W.
            // BSWAP defaults to 32 bits; 0x66 does not select a 16-bit swap.
            if i.opcode() == yaxpeax_x86::long_mode::Opcode::BSWAP
                && !i.prefixes.rex().is_some_and(|r| r.w())
            {
                if let Value::Register { width, .. } = &mut value {
                    *width = 4;
                }
            }
            return Ok(value);
        }
        Operand::ImmediateI8 { imm } => return Ok(Value::Immediate(imm as i64 as u64)),
        Operand::ImmediateU8 { imm } => return Ok(Value::Immediate(imm as u64)),
        Operand::ImmediateI16 { imm } => return Ok(Value::Immediate(imm as i64 as u64)),
        Operand::ImmediateU16 { imm } => return Ok(Value::Immediate(imm as u64)),
        Operand::ImmediateI32 { imm } => return Ok(Value::Immediate(imm as i64 as u64)),
        Operand::ImmediateU32 { imm } => return Ok(Value::Immediate(imm as u64)),
        Operand::ImmediateI64 { imm } => return Ok(Value::Immediate(imm as u64)),
        Operand::ImmediateU64 { imm } => return Ok(Value::Immediate(imm)),
        Operand::AbsoluteU32 { addr } => {
            if i.prefixes.address_size() {
                addr as u64
            } else {
                addr as i32 as i64 as u64
            }
        }
        Operand::AbsoluteU64 { addr } => addr,
        Operand::MemDeref { base } => address_reg(cpu, base, next).ok_or_else(bad)?,
        Operand::Disp { base, disp } => address_reg(cpu, base, next)
            .ok_or_else(bad)?
            .wrapping_add(disp as i64 as u64),
        Operand::MemIndexScale { index, scale } => address_reg(cpu, index, next)
            .ok_or_else(bad)?
            .wrapping_mul(scale as u64),
        Operand::MemIndexScaleDisp { index, scale, disp } => address_reg(cpu, index, next)
            .ok_or_else(bad)?
            .wrapping_mul(scale as u64)
            .wrapping_add(disp as i64 as u64),
        Operand::MemBaseIndexScale { base, index, scale } => {
            address_reg(cpu, base, next).ok_or_else(bad)?.wrapping_add(
                address_reg(cpu, index, next)
                    .ok_or_else(bad)?
                    .wrapping_mul(scale as u64),
            )
        }
        Operand::MemBaseIndexScaleDisp {
            base,
            index,
            scale,
            disp,
        } => address_reg(cpu, base, next)
            .ok_or_else(bad)?
            .wrapping_add(
                address_reg(cpu, index, next)
                    .ok_or_else(bad)?
                    .wrapping_mul(scale as u64),
            )
            .wrapping_add(disp as i64 as u64),
        _ => return Err(bad()),
    };
    let addr = if i.prefixes.address_size() {
        addr as u32 as u64
    } else {
        addr
    };
    let segment = if lea {
        0
    } else {
        match i.segment_override_for_op(index) {
            Some(Segment::FS) => cpu.fs_base,
            Some(Segment::GS) => return Err(bad()),
            _ => 0,
        }
    };
    let width = if i.opcode() == yaxpeax_x86::long_mode::Opcode::MOVSXD
        && !i.prefixes.rex().is_some_and(|r| r.w())
        && i.prefixes.operand_size()
    {
        2
    } else if matches!(
        i.opcode(),
        yaxpeax_x86::long_mode::Opcode::CVTDQ2PD | yaxpeax_x86::long_mode::Opcode::CVTPS2PD
    ) {
        // These packed conversions read two 32-bit elements, not m128.
        // yaxpeax 2.2.0 reports the destination vector width for memory operands.
        8
    } else if lea {
        8
    } else {
        i.mem_size().and_then(|s| s.bytes_size()).ok_or_else(bad)? as usize
    };
    if !matches!(width, 1 | 2 | 4 | 8 | 10 | 16) {
        return Err(bad());
    }
    Ok(Value::Memory {
        address: addr.wrapping_add(segment),
        width,
    })
}
