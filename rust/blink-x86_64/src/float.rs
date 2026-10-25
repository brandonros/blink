//! SSE floating point through host-independent IEEE arithmetic. Guest MXCSR
//! controls rounding, denormal handling and sticky exceptions; host FP state is
//! never changed. Unsupported instructions still trap explicitly.
use super::*;
use rustc_apfloat::{
    ieee::{Double, IeeeFloat, Semantics, Single},
    Float, FloatConvert, Round, Status,
};
// Keep the source precision but widen the exponent to distinguish an ordinary
// rounded maximum from overflow saturated to the maximum. APFloat's directed
// overflow_result returns only INEXACT in the latter case; x86 also sets OE.
struct WideSingle;
impl Semantics for WideSingle {
    const BITS: usize = 33;
    const EXP_BITS: usize = 9;
}
struct WideDouble;
impl Semantics for WideDouble {
    const BITS: usize = 65;
    const EXP_BITS: usize = 12;
}

fn rounding(mxcsr: u32) -> Round {
    match (mxcsr >> 13) & 3 {
        0 => Round::NearestTiesToEven,
        1 => Round::TowardNegative,
        2 => Round::TowardPositive,
        _ => Round::TowardZero,
    }
}
fn input<F: Float>(bits: u64, mxcsr: u32, flags: &mut u32) -> F {
    let value = F::from_bits(bits as u128);
    if value.is_denormal() {
        if mxcsr & 64 != 0 {
            return F::ZERO.copy_sign(value);
        }
        *flags |= 2;
    }
    value
}
fn exceptions(status: Status) -> u32 {
    u32::from(status.contains(Status::INVALID_OP))
        | (u32::from(status.contains(Status::DIV_BY_ZERO)) << 2)
        | (u32::from(status.contains(Status::OVERFLOW)) << 3)
        | (u32::from(status.contains(Status::UNDERFLOW)) << 4)
        | (u32::from(status.contains(Status::INEXACT)) << 5)
}
fn compare<F: Float>(
    left: u64,
    right: u64,
    mxcsr: u32,
    quiet: bool,
    flags: &mut u32,
) -> Option<std::cmp::Ordering> {
    let mut inputs = 0;
    let a = input::<F>(left, mxcsr, &mut inputs);
    let b = input::<F>(right, mxcsr, &mut inputs);
    if a.is_nan() || b.is_nan() {
        if !quiet || a.is_signaling() || b.is_signaling() {
            *flags |= 1;
        }
        None
    } else {
        *flags |= inputs;
        a.partial_cmp(&b)
    }
}
fn to_integer<F: Float>(
    bits: u64,
    width: usize,
    mxcsr: u32,
    truncate: bool,
    flags: &mut u32,
) -> u64 {
    // SSE float-to-integer conversion does not signal a denormal operand.
    let input = input::<F>(bits, mxcsr, &mut 0);
    let result = input.to_i128_r(
        width,
        if truncate {
            Round::TowardZero
        } else {
            rounding(mxcsr)
        },
        &mut false,
    );
    *flags |= exceptions(result.status);
    if result.status.contains(Status::INVALID_OP) {
        1 << (width - 1)
    } else {
        result.value as u64
    }
}
fn from_integer<F: Float>(value: i128, mxcsr: u32, flags: &mut u32) -> u64 {
    let result = F::from_i128_r(value, rounding(mxcsr));
    *flags |= exceptions(result.status);
    result.value.to_bits() as u64
}
fn convert_float<F: Float + FloatConvert<G> + FloatConvert<W>, G: Float, W: Float>(
    bits: u64,
    mxcsr: u32,
    flags: &mut u32,
) -> u64 {
    let value = input::<F>(bits, mxcsr, flags);
    let result: rustc_apfloat::StatusAnd<G> = value.convert_r(rounding(mxcsr), &mut false);
    *flags |= exceptions(result.status);
    let mut output = result.value;
    if output.abs() == G::largest() && result.status.contains(Status::INEXACT) {
        let wide: W = value.convert_r(rounding(mxcsr), &mut false).value;
        if wide.abs() >= W::from_u128(1).value.scalbn(G::MAX_EXP + 1) {
            *flags |= 8;
        }
    }
    if mxcsr & (1 << 15) != 0 && output.is_denormal() {
        *flags |= 0x30;
        output = G::ZERO.copy_sign(output);
    }
    output.to_bits() as u64
}
fn operation<F: Float>(op: Op, a: F, b: F, round: Round) -> rustc_apfloat::StatusAnd<F> {
    match op {
        Op::ADDSS | Op::ADDSD | Op::ADDPS | Op::ADDPD => a.add_r(b, round),
        Op::SUBSS | Op::SUBSD | Op::SUBPS | Op::SUBPD => a.sub_r(b, round),
        Op::MULSS | Op::MULSD | Op::MULPS | Op::MULPD => a.mul_r(b, round),
        _ => a.div_r(b, round),
    }
}
fn arithmetic<F: Float + FloatConvert<W>, W: Float>(
    op: Op,
    left: u64,
    right: u64,
    mxcsr: u32,
    flags: &mut u32,
) -> u64 {
    let mut inputs = 0;
    let a = input::<F>(left, mxcsr, &mut inputs);
    let b = input::<F>(right, mxcsr, &mut inputs);
    if matches!(
        op,
        Op::MINSS
            | Op::MINSD
            | Op::MINPS
            | Op::MINPD
            | Op::MAXSS
            | Op::MAXSD
            | Op::MAXPS
            | Op::MAXPD
    ) {
        let order = compare::<F>(left, right, mxcsr, false, flags);
        let minimum = matches!(op, Op::MINSS | Op::MINSD | Op::MINPS | Op::MINPD);
        let choose_a = if minimum {
            order == Some(std::cmp::Ordering::Less)
        } else {
            order == Some(std::cmp::Ordering::Greater)
        };
        return if choose_a { a.to_bits() } else { b.to_bits() } as u64;
    }
    let rounded = operation(op, a, b, rounding(mxcsr));
    if !a.is_nan()
        && !b.is_nan()
        && !rounded
            .status
            .intersects(Status::INVALID_OP | Status::DIV_BY_ZERO)
    {
        *flags |= inputs;
    }
    *flags |= exceptions(rounded.status);
    let mut value = rounded.value;
    if value.abs() == F::largest() && rounded.status.contains(Status::INEXACT) {
        let wide_a: W = a.convert_r(Round::NearestTiesToEven, &mut false).value;
        let wide_b: W = b.convert_r(Round::NearestTiesToEven, &mut false).value;
        let wide = operation(op, wide_a, wide_b, rounding(mxcsr)).value;
        if wide.abs() >= W::from_u128(1).value.scalbn(F::MAX_EXP + 1) {
            *flags |= 8;
        }
    }
    // x86's masked invalid-operation result is the negative indefinite NaN.
    // Payload-bearing operands retain their payload with the quiet bit set.
    if value.is_nan() {
        if a.is_nan() {
            return (a.to_bits() | (1 << (F::PRECISION - 2))) as u64;
        }
        if b.is_nan() {
            return (b.to_bits() | (1 << (F::PRECISION - 2))) as u64;
        }
        value = -F::NAN;
    }
    if mxcsr & (1 << 15) != 0 && value.is_denormal() {
        *flags |= 0x30;
        value = F::ZERO.copy_sign(value);
    }
    value.to_bits() as u64
}
impl X86 {
    fn fp_exceptions(&mut self, flags: u32) -> Result<()> {
        self.mxcsr |= flags;
        if flags & !(self.mxcsr >> 7) & 63 != 0 {
            return Err(Error::ArithmeticFault);
        }
        Ok(())
    }
    pub(super) fn float(
        &mut self,
        m: &mut Memory,
        op: Op,
        a: Option<Value>,
        b: Option<Value>,
        c: Option<Value>,
        pc: u64,
    ) -> Result<bool> {
        let bad = || Error::UnsupportedInstruction {
            pc,
            encoding: op as u32,
        };
        match op {
            Op::LDMXCSR => {
                let value = self.read(m, a.ok_or_else(bad)?)? as u32;
                if value & !0xffff != 0 {
                    return Err(Error::MemoryFault {
                        address: pc,
                        access: blink_core::Access::Read,
                    });
                }
                self.mxcsr = value;
            }
            Op::STMXCSR => self.write(m, a.ok_or_else(bad)?, self.mxcsr as u64)?,
            Op::CVTSS2SD | Op::CVTSD2SS | Op::CVTPS2PD | Op::CVTPD2PS => {
                let dst = a.ok_or_else(bad)?;
                let src = self.vector(m, b.ok_or_else(bad)?)?;
                let mut out = if matches!(op, Op::CVTPS2PD | Op::CVTPD2PS) {
                    [0; 16]
                } else {
                    self.vector(m, dst)?
                };
                let count = if matches!(op, Op::CVTPS2PD | Op::CVTPD2PS) {
                    2
                } else {
                    1
                };
                let mut flags = 0;
                for n in 0..count {
                    if matches!(op, Op::CVTSS2SD | Op::CVTPS2PD) {
                        let bits =
                            u32::from_le_bytes(src[n * 4..n * 4 + 4].try_into().unwrap()) as u64;
                        let result = convert_float::<Single, Double, IeeeFloat<WideDouble>>(
                            bits, self.mxcsr, &mut flags,
                        );
                        out[n * 8..n * 8 + 8].copy_from_slice(&result.to_le_bytes());
                    } else {
                        let bits = u64::from_le_bytes(src[n * 8..n * 8 + 8].try_into().unwrap());
                        let result = convert_float::<Double, Single, IeeeFloat<WideSingle>>(
                            bits, self.mxcsr, &mut flags,
                        );
                        out[n * 4..n * 4 + 4].copy_from_slice(&result.to_le_bytes()[..4]);
                    }
                }
                self.fp_exceptions(flags)?;
                self.write_vector(m, dst, out)?;
            }
            Op::CVTSS2SI | Op::CVTSD2SI | Op::CVTTSS2SI | Op::CVTTSD2SI => {
                let dst = a.ok_or_else(bad)?;
                let bytes = self.vector(m, b.ok_or_else(bad)?)?;
                let mut flags = 0;
                let truncate = matches!(op, Op::CVTTSS2SI | Op::CVTTSD2SI);
                let value = if matches!(op, Op::CVTSS2SI | Op::CVTTSS2SI) {
                    to_integer::<Single>(
                        u32::from_le_bytes(bytes[..4].try_into().unwrap()) as u64,
                        dst.width() * 8,
                        self.mxcsr,
                        truncate,
                        &mut flags,
                    )
                } else {
                    to_integer::<Double>(
                        u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                        dst.width() * 8,
                        self.mxcsr,
                        truncate,
                        &mut flags,
                    )
                };
                self.fp_exceptions(flags)?;
                self.write(m, dst, value)?;
            }
            Op::CVTSI2SS | Op::CVTSI2SD => {
                let dst = a.ok_or_else(bad)?;
                let src = b.ok_or_else(bad)?;
                let mut bytes = self.vector(m, dst)?;
                let integer = signed(self.read(m, src)?, src.width());
                let mut flags = 0;
                let (value, width) = if op == Op::CVTSI2SS {
                    (from_integer::<Single>(integer, self.mxcsr, &mut flags), 4)
                } else {
                    (from_integer::<Double>(integer, self.mxcsr, &mut flags), 8)
                };
                self.fp_exceptions(flags)?;
                bytes[..width].copy_from_slice(&value.to_le_bytes()[..width]);
                self.write_vector(m, dst, bytes)?;
            }
            Op::COMISS | Op::UCOMISS | Op::COMISD | Op::UCOMISD => {
                let l = self.vector(m, a.ok_or_else(bad)?)?;
                let r = self.vector(m, b.ok_or_else(bad)?)?;
                let mut flags = 0;
                let quiet = matches!(op, Op::UCOMISS | Op::UCOMISD);
                let order = if matches!(op, Op::COMISS | Op::UCOMISS) {
                    compare::<Single>(
                        u32::from_le_bytes(l[..4].try_into().unwrap()) as u64,
                        u32::from_le_bytes(r[..4].try_into().unwrap()) as u64,
                        self.mxcsr,
                        quiet,
                        &mut flags,
                    )
                } else {
                    compare::<Double>(
                        u64::from_le_bytes(l[..8].try_into().unwrap()),
                        u64::from_le_bytes(r[..8].try_into().unwrap()),
                        self.mxcsr,
                        quiet,
                        &mut flags,
                    )
                };
                self.fp_exceptions(flags)?;
                self.flags = (self.flags & !0x8d5)
                    | match order {
                        None => 0x45,
                        Some(std::cmp::Ordering::Less) => 1,
                        Some(std::cmp::Ordering::Equal) => 64,
                        _ => 0,
                    };
            }
            Op::CMPSS | Op::CMPSD | Op::CMPPS | Op::CMPPD => {
                let dst = a.ok_or_else(bad)?;
                let mut out = self.vector(m, dst)?;
                let right = self.vector(m, b.ok_or_else(bad)?)?;
                let predicate = self.read(m, c.ok_or_else(bad)?)? & 7;
                let quiet = matches!(predicate, 0 | 3 | 4 | 7);
                let lane = if matches!(op, Op::CMPSD | Op::CMPPD) {
                    8
                } else {
                    4
                };
                let count = if matches!(op, Op::CMPSS | Op::CMPSD) {
                    lane
                } else {
                    16
                };
                let mut flags = 0;
                for offset in (0..count).step_by(lane) {
                    let mut l = [0; 8];
                    let mut r = [0; 8];
                    l[..lane].copy_from_slice(&out[offset..offset + lane]);
                    r[..lane].copy_from_slice(&right[offset..offset + lane]);
                    let order = if lane == 4 {
                        compare::<Single>(
                            u64::from_le_bytes(l),
                            u64::from_le_bytes(r),
                            self.mxcsr,
                            quiet,
                            &mut flags,
                        )
                    } else {
                        compare::<Double>(
                            u64::from_le_bytes(l),
                            u64::from_le_bytes(r),
                            self.mxcsr,
                            quiet,
                            &mut flags,
                        )
                    };
                    use std::cmp::Ordering::*;
                    let yes = match predicate {
                        0 => order == Some(Equal),
                        1 => order == Some(Less),
                        2 => matches!(order, Some(Less | Equal)),
                        3 => order.is_none(),
                        4 => order != Some(Equal),
                        5 => order != Some(Less),
                        6 => !matches!(order, Some(Less | Equal)),
                        _ => order.is_some(),
                    };
                    out[offset..offset + lane].fill(if yes { 255 } else { 0 });
                }
                self.fp_exceptions(flags)?;
                self.write_vector(m, dst, out)?;
            }
            Op::ADDSS
            | Op::ADDSD
            | Op::ADDPS
            | Op::ADDPD
            | Op::SUBSS
            | Op::SUBSD
            | Op::SUBPS
            | Op::SUBPD
            | Op::MULSS
            | Op::MULSD
            | Op::MULPS
            | Op::MULPD
            | Op::DIVSS
            | Op::DIVSD
            | Op::DIVPS
            | Op::DIVPD
            | Op::MINSS
            | Op::MINSD
            | Op::MINPS
            | Op::MINPD
            | Op::MAXSS
            | Op::MAXSD
            | Op::MAXPS
            | Op::MAXPD => {
                let dst = a.ok_or_else(bad)?;
                let Value::Vector(_) = dst else {
                    return Err(bad());
                };
                let mut out = self.vector(m, dst)?;
                let right = self.vector(m, b.ok_or_else(bad)?)?;
                let lane = if matches!(
                    op,
                    Op::ADDSD
                        | Op::ADDPD
                        | Op::SUBSD
                        | Op::SUBPD
                        | Op::MULSD
                        | Op::MULPD
                        | Op::DIVSD
                        | Op::DIVPD
                        | Op::MINSD
                        | Op::MINPD
                        | Op::MAXSD
                        | Op::MAXPD
                ) {
                    8
                } else {
                    4
                };
                let count = if matches!(
                    op,
                    Op::ADDSS
                        | Op::ADDSD
                        | Op::SUBSS
                        | Op::SUBSD
                        | Op::MULSS
                        | Op::MULSD
                        | Op::DIVSS
                        | Op::DIVSD
                        | Op::MINSS
                        | Op::MINSD
                        | Op::MAXSS
                        | Op::MAXSD
                ) {
                    lane
                } else {
                    16
                };
                let mut flags = 0;
                for offset in (0..count).step_by(lane) {
                    let mut l = [0; 8];
                    let mut r = [0; 8];
                    l[..lane].copy_from_slice(&out[offset..offset + lane]);
                    r[..lane].copy_from_slice(&right[offset..offset + lane]);
                    let result = if lane == 4 {
                        arithmetic::<Single, IeeeFloat<WideSingle>>(
                            op,
                            u64::from_le_bytes(l),
                            u64::from_le_bytes(r),
                            self.mxcsr,
                            &mut flags,
                        )
                    } else {
                        arithmetic::<Double, IeeeFloat<WideDouble>>(
                            op,
                            u64::from_le_bytes(l),
                            u64::from_le_bytes(r),
                            self.mxcsr,
                            &mut flags,
                        )
                    };
                    out[offset..offset + lane].copy_from_slice(&result.to_le_bytes()[..lane]);
                }
                self.fp_exceptions(flags)?;
                self.write_vector(m, dst, out)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blink_core::Permissions;
    fn machine(code: &[u8]) -> (X86, Memory) {
        let mut memory = Memory::new(8192);
        memory.map(0x1000, 4096, Permissions::RX).unwrap();
        memory.map(0x2000, 4096, Permissions::RW).unwrap();
        memory.initialize(0x1000, code).unwrap();
        (X86::new(0x1000, 0x3000), memory)
    }
    #[test]
    fn scalar_memory_at_page_end_preserves_high_lanes() {
        // addss (%rax), %xmm0 -- only four bytes are readable.
        let (mut cpu, mut memory) = machine(&[0xf3, 0x0f, 0x58, 0x00]);
        cpu.regs[0] = 0x2ffc;
        cpu.xmm[0] = [0xa5; 16];
        cpu.xmm[0][..4].copy_from_slice(&1.0f32.to_bits().to_le_bytes());
        memory.store(0x2ffc, 2.0f32.to_bits() as u64, 4).unwrap();
        cpu.step(&mut memory).unwrap();
        assert_eq!(&cpu.xmm[0][..4], &3.0f32.to_bits().to_le_bytes());
        assert_eq!(cpu.xmm[0][4..], [0xa5; 12]);
    }
    #[test]
    fn packed_exceptions_accumulate_but_unmasked_fault_does_not_write_destination() {
        let (mut cpu, mut memory) = machine(&[0x0f, 0x5e, 0xc1]); // divps xmm0,xmm1
        for n in 0..4 {
            cpu.xmm[0][n * 4..n * 4 + 4].copy_from_slice(&1.0f32.to_bits().to_le_bytes());
            cpu.xmm[1][n * 4..n * 4 + 4].copy_from_slice(&2.0f32.to_bits().to_le_bytes());
        }
        cpu.xmm[1][4..8].fill(0);
        let original = cpu.xmm[0];
        cpu.mxcsr &= !(1 << 9); // unmask divide by zero
        assert!(matches!(cpu.step(&mut memory), Err(Error::ArithmeticFault)));
        assert_eq!(cpu.xmm[0], original);
        assert_ne!(cpu.mxcsr & 4, 0);
        cpu.rip = 0x1000;
        cpu.mxcsr = 0x1f80;
        cpu.step(&mut memory).unwrap();
        assert_eq!(&cpu.xmm[0][4..8], &f32::INFINITY.to_bits().to_le_bytes());
        for n in [0, 2, 3] {
            assert_eq!(
                &cpu.xmm[0][n * 4..n * 4 + 4],
                &0.5f32.to_bits().to_le_bytes()
            );
        }
        assert_eq!(cpu.mxcsr, 0x1f84);
    }
}
