//! Guest-owned x87 stack and software 80-bit arithmetic. No host FP environment.
use super::*;
use rustc_apfloat::{
    ieee::{Double, IeeeFloat, Semantics, Single, X87DoubleExtended as Extended},
    Float, FloatConvert, Round, Status,
};
struct WideExtended;
impl Semantics for WideExtended {
    const BITS: usize = 80;
    const EXP_BITS: usize = 16;
}
fn calculate<F: Float>(op: Op, x: F, y: F, round: Round) -> rustc_apfloat::StatusAnd<F> {
    match op {
        Op::FADD | Op::FADDP | Op::FIADD => x.add_r(y, round),
        Op::FMUL | Op::FMULP | Op::FIMUL => x.mul_r(y, round),
        Op::FSUB | Op::FSUBP | Op::FSUBR | Op::FSUBRP | Op::FISUB | Op::FISUBR => x.sub_r(y, round),
        _ => x.div_r(y, round),
    }
}
// A wider exponent covers the complete range of operations on two 80-bit
// operands. Truncating to 111 bits and jamming discarded bits into the low bit
// avoids double rounding when converting to the 24/53-bit precision controls.
struct Intermediate;
impl Semantics for Intermediate {
    const BITS: usize = 128;
    const EXP_BITS: usize = 17;
}
struct Precision24;
impl Semantics for Precision24 {
    const BITS: usize = 39;
    const EXP_BITS: usize = 15;
}
struct Precision53;
impl Semantics for Precision53 {
    const BITS: usize = 68;
    const EXP_BITS: usize = 15;
}
// Extended-exponent formats retain the selected significand precision while
// computing the scaled result required by unmasked overflow/underflow traps.
struct Trap24;
impl Semantics for Trap24 {
    const BITS: usize = 41;
    const EXP_BITS: usize = 17;
}
struct Trap53;
impl Semantics for Trap53 {
    const BITS: usize = 70;
    const EXP_BITS: usize = 17;
}
struct Trap64;
impl Semantics for Trap64 {
    const BITS: usize = 81;
    const EXP_BITS: usize = 17;
}
fn wrapped_arithmetic<F>(
    op: Op,
    x: Extended,
    y: Extended,
    control: u16,
    round: Round,
) -> Option<(u128, u16, bool)>
where
    F: Float + FloatConvert<Extended>,
    IeeeFloat<Intermediate>: FloatConvert<F>,
{
    let wx: IeeeFloat<Intermediate> = x.convert(&mut false).value;
    let wy: IeeeFloat<Intermediate> = y.convert(&mut false).value;
    let wide = calculate(op, wx, wy, Round::TowardZero);
    let mut exact = wide.value;
    if wide.status.contains(Status::INEXACT) {
        exact = IeeeFloat::<Intermediate>::from_bits(exact.to_bits() | 1);
    }
    let result: rustc_apfloat::StatusAnd<F> = exact.convert_r(round, &mut false);
    let flag = if control & 8 == 0 && result.value.abs() >= F::from_u128(1).value.scalbn(16384) {
        8
    } else if control & 16 == 0
        && !result.value.is_zero()
        && result.value.abs() < F::from_u128(1).value.scalbn(-16382)
    {
        16
    } else {
        return None;
    };
    let scaled = result.value.scalbn(if flag == 8 { -24576 } else { 24576 });
    let converted: Extended = scaled.convert(&mut false).value;
    let truncated: F = exact.convert_r(Round::TowardZero, &mut false).value;
    let up = result.status.contains(Status::INEXACT) && result.value.abs() > truncated.abs();
    Some((converted.to_bits(), flag | flags(result.status), up))
}
fn reduced<F>(
    op: Op,
    x: Extended,
    y: Extended,
    round: Round,
) -> (rustc_apfloat::StatusAnd<Extended>, bool)
where
    IeeeFloat<Intermediate>: FloatConvert<F>,
    F: Float + FloatConvert<Extended> + FloatConvert<IeeeFloat<Intermediate>>,
{
    let wx: IeeeFloat<Intermediate> = x.convert(&mut false).value;
    let wy: IeeeFloat<Intermediate> = y.convert(&mut false).value;
    let intermediate = calculate(op, wx, wy, Round::TowardZero);
    let mut value = intermediate.value;
    // Exact cancellation has a rounding-dependent zero sign.
    if value.is_zero() && !intermediate.status.contains(Status::INEXACT) {
        value = calculate(op, wx, wy, round).value;
    }
    if intermediate.status.contains(Status::INEXACT) {
        value = IeeeFloat::<Intermediate>::from_bits(value.to_bits() | 1);
    }
    let converted: rustc_apfloat::StatusAnd<F> = value.convert_r(round, &mut false);
    let mut status = converted.status | intermediate.status;
    let result: Extended = converted.value.convert(&mut false).value;
    if converted.value.abs() == F::largest()
        && status.contains(Status::INEXACT)
        && value.abs()
            >= IeeeFloat::<Intermediate>::from_u128(1)
                .value
                .scalbn(F::MAX_EXP + 1)
    {
        status |= Status::OVERFLOW;
    }
    let widened: IeeeFloat<Intermediate> = converted.value.convert(&mut false).value;
    (
        rustc_apfloat::StatusAnd {
            status,
            value: result,
        },
        widened.abs() > value.abs(),
    )
}
// Scale the 64-bit significand to a 128-bit integer radicand. The exact
// remainder supports all precision controls without an intermediate FP round.
fn square_root(bits: u128, control: u16) -> (u128, u16, bool) {
    let value = Extended::from_bits(bits);
    let mut significand = bits as u64 as u128;
    let raw_exp = ((bits >> 64) & 0x7fff) as i32;
    if raw_exp != 0 && significand & (1 << 63) == 0 {
        return (INDEFINITE, 1, false);
    }
    if value.is_nan() {
        return (bits | (1 << 62), u16::from(value.is_signaling()), false);
    }
    if value.is_zero() {
        return (bits, 0, false);
    }
    if value.is_negative() {
        return (INDEFINITE, 1, false);
    }
    if value.is_infinite() {
        return (bits, 0, false);
    }
    let mut exceptions = if raw_exp == 0 { 2 } else { 0 };
    let mut exponent = raw_exp.max(1) - 16383;
    while significand & (1 << 63) == 0 {
        significand <<= 1;
        exponent -= 1;
    }
    if exponent & 1 != 0 {
        significand <<= 1;
        exponent -= 1;
    }
    let (root, remainder) = super::float::integer_sqrt(significand << 63);
    let precision = match (control >> 8) & 3 {
        0 => 24,
        2 => 53,
        _ => 64,
    };
    let shift = 64 - precision;
    let tail = root & ((1u128 << shift) - 1);
    let mut rounded = root >> shift;
    let inexact = tail != 0 || remainder != 0;
    let up = inexact
        && match (control >> 10) & 3 {
            0 if shift == 0 => remainder > root,
            0 => {
                let halfway = 1u128 << (shift - 1);
                tail > halfway || (tail == halfway && (remainder != 0 || rounded & 1 != 0))
            }
            2 => true,
            _ => false,
        };
    rounded += u128::from(up);
    rounded <<= shift;
    let mut output_exp = exponent / 2 + 16383;
    if rounded & (1 << 64) != 0 {
        rounded >>= 1;
        output_exp += 1;
    }
    if inexact {
        exceptions |= 32;
    }
    (rounded | ((output_exp as u128) << 64), exceptions, up)
}
// Arithmetic-only format: 126 significand bits and a wide exponent. Never
// serialize it with to_bits/from_bits (the public bit container is only u128).
struct Transcendental;
impl Semantics for Transcendental {
    const BITS: usize = 143;
    const EXP_BITS: usize = 17;
}
type Math = IeeeFloat<Transcendental>;
fn pi() -> Math {
    Math::from_u128(0xc90f_daa2_2168_c234_c4c6_628b_80dc_1cd1)
        .value
        .scalbn(-126)
}

fn atan_series(mut z: Math) -> Math {
    let one = Math::from_u128(1).value;
    let reduced = z > one.scalbn(-1);
    if reduced {
        z = (z - one)
            .value
            .div_r((z + one).value, Round::NearestTiesToEven)
            .value;
    }
    let square = -(z * z).value;
    let mut power = z;
    let mut sum = z;
    for n in 1..256 {
        power = (power * square).value;
        let term = (power / Math::from_u128(2 * n + 1).value).value;
        let next = (sum + term).value;
        if next == sum {
            break;
        }
        sum = next;
    }
    if reduced {
        (pi().scalbn(-2) + sum).value
    } else {
        sum
    }
}
fn atan2(y: Extended, x: Extended, round: Round, control: u16) -> (u128, u16, bool) {
    let wy: Math = y.abs().convert(&mut false).value;
    let wx: Math = x.abs().convert(&mut false).value;
    let mut angle = if y.is_zero() || (x.is_infinite() && !y.is_infinite()) {
        Math::ZERO
    } else if x.is_zero() || (y.is_infinite() && !x.is_infinite()) {
        pi().scalbn(-1)
    } else if x.is_infinite() && y.is_infinite() {
        pi().scalbn(-2)
    } else if wy > wx {
        (pi().scalbn(-1) - atan_series((wx / wy).value)).value
    } else {
        atan_series((wy / wx).value)
    };
    if x.is_negative() {
        angle = (pi() - angle).value;
    }
    if y.is_negative() {
        angle = -angle;
    }
    transcend_result(angle, round, !angle.is_zero(), control)
}
fn transcend_result(
    mut value: Math,
    round: Round,
    precision: bool,
    control: u16,
) -> (u128, u16, bool) {
    let preview: rustc_apfloat::StatusAnd<Extended> = value.convert_r(round, &mut false);
    let trap = if control & 8 == 0
        && (preview.status.contains(Status::OVERFLOW)
            || value.abs() >= Math::from_u128(1).value.scalbn(16384))
    {
        value = value.scalbn(-24576);
        8
    } else if control & 16 == 0
        && !value.is_zero()
        && value.abs() < Math::from_u128(1).value.scalbn(-16382)
    {
        value = value.scalbn(24576);
        16
    } else {
        0
    };
    let result: rustc_apfloat::StatusAnd<Extended> = value.convert_r(round, &mut false);
    let widened: Math = result.value.convert(&mut false).value;
    let mut exceptions = flags(result.status) | trap;
    if precision {
        exceptions |= 32;
    }
    if exceptions & 32 != 0 && value.abs() < Math::from_u128(1).value.scalbn(-16382) {
        exceptions |= 16;
    }
    if result.value.abs() == Extended::largest()
        && value.abs() >= Math::from_u128(1).value.scalbn(16384)
    {
        exceptions |= 8;
    }
    (
        result.value.to_bits(),
        exceptions,
        widened.abs() > value.abs(),
    )
}
fn ln2() -> Math {
    Math::from_u128(0xb172_17f7_d1cf_79ab_c9e3_b398_03f2_f6af)
        .value
        .scalbn(-128)
}
fn exp2_minus_one(x: Extended, round: Round, control: u16) -> (u128, u16, bool) {
    let one = Extended::from_u128(1).value;
    if x.abs() == one {
        return (
            if x.is_negative() {
                (-one.scalbn(-1)).to_bits()
            } else {
                one.to_bits()
            },
            32,
            false,
        );
    }
    let wx: Math = x.convert(&mut false).value;
    let z = (wx * ln2()).value;
    let mut term = z;
    let mut sum = z;
    for n in 2..256 {
        term = ((term * z).value / Math::from_u128(n).value).value;
        let next = (sum + term).value;
        if next == sum {
            break;
        }
        sum = next;
    }
    transcend_result(sum, round, true, control)
}

fn log_series(z: Math) -> Math {
    let square = (z * z).value;
    let mut power = z;
    let mut sum = z;
    for n in 1..256 {
        power = (power * square).value;
        let next = (sum + (power / Math::from_u128(2 * n + 1).value).value).value;
        if next == sum {
            break;
        }
        sum = next;
    }
    (sum.scalbn(1) / ln2()).value
}
fn log2_positive(x: Math) -> Math {
    let exponent = x.ilogb();
    let one = Math::from_u128(1).value;
    let mantissa = x.scalbn(-exponent);
    (Math::from_i128(exponent as i128).value
        + log_series(((mantissa - one).value / (mantissa + one).value).value))
    .value
}
fn log2_plus_one(x: Math) -> Math {
    let one = Math::from_u128(1).value;
    if x.abs() <= one.scalbn(-1) {
        log_series((x / (one.scalbn(1) + x).value).value)
    } else {
        log2_positive((one + x).value)
    }
}
fn sin_cos(x: Extended) -> (Math, Math) {
    let wx: Math = x.convert(&mut false).value;
    let half_pi = pi().scalbn(-1);
    let quadrant = (wx / half_pi)
        .value
        .round_to_integral(Round::NearestTiesToEven)
        .value
        .to_i128_r(64, Round::TowardZero, &mut false)
        .value;
    let r = (wx - (Math::from_i128(quadrant).value * half_pi).value).value;
    let square = -(r * r).value;
    let mut sine = r;
    let mut cosine = Math::from_u128(1).value;
    let mut sin_term = sine;
    let mut cos_term = cosine;
    for n in 1..128 {
        sin_term = ((sin_term * square).value / Math::from_u128((2 * n) * (2 * n + 1)).value).value;
        cos_term = ((cos_term * square).value / Math::from_u128((2 * n - 1) * (2 * n)).value).value;
        let next_sin = (sine + sin_term).value;
        let next_cos = (cosine + cos_term).value;
        if next_sin == sine && next_cos == cosine {
            break;
        }
        sine = next_sin;
        cosine = next_cos;
    }
    match quadrant.rem_euclid(4) {
        0 => (sine, cosine),
        1 => (cosine, -sine),
        2 => (-sine, -cosine),
        _ => (-cosine, sine),
    }
}
const INDEFINITE: u128 = 0xffff_c000_0000_0000_0000;
fn unsupported_extended(bits: u128) -> bool {
    ((bits >> 64) & 0x7fff) != 0 && bits & (1 << 63) == 0
}
fn denormal_extended(bits: u128) -> bool {
    ((bits >> 64) & 0x7fff) == 0 && bits as u64 != 0
}
fn arithmetic_input(bits: u128, exceptions: &mut u16) -> Extended {
    if unsupported_extended(bits) {
        *exceptions |= 1;
        Extended::from_bits(INDEFINITE)
    } else {
        if denormal_extended(bits) {
            *exceptions |= 2;
        }
        Extended::from_bits(bits)
    }
}
fn value_tag(bits: u128) -> u8 {
    let exponent = (bits >> 64) & 0x7fff;
    let fraction = bits as u64;
    if exponent == 0 && fraction == 0 {
        1
    } else if exponent == 0 || exponent == 0x7fff || fraction & (1 << 63) == 0 {
        2
    } else {
        0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fpu {
    pub values: [u128; 8],
    pub tags: [u8; 8],
    pub top: usize,
    pub control: u16,
    pub status: u16,
    pub opcode: u16,
    pub instruction: u64,
    pub data: u64,
}
impl Default for Fpu {
    fn default() -> Self {
        Self {
            values: [0; 8],
            tags: [3; 8],
            top: 0,
            control: 0x37f,
            status: 0,
            opcode: 0,
            instruction: 0,
            data: 0,
        }
    }
}
fn flags(status: Status) -> u16 {
    u16::from(status.contains(Status::INVALID_OP))
        | (u16::from(status.contains(Status::DIV_BY_ZERO)) << 2)
        | (u16::from(status.contains(Status::OVERFLOW)) << 3)
        | (u16::from(status.contains(Status::UNDERFLOW)) << 4)
        | (u16::from(status.contains(Status::INEXACT)) << 5)
}
impl Fpu {
    fn round(&self) -> Round {
        match (self.control >> 10) & 3 {
            0 => Round::NearestTiesToEven,
            1 => Round::TowardNegative,
            2 => Round::TowardPositive,
            _ => Round::TowardZero,
        }
    }
    pub fn status(&self) -> u16 {
        (self.status & !0x3800) | ((self.top as u16) << 11)
    }
    fn exception(&mut self, mut bits: u16) -> bool {
        if bits & !self.control & 2 != 0 {
            // An unmasked denormal operand prevents result computation.
            bits &= !56;
            self.status &= !0x200;
        }
        self.status |= bits;
        if self.status & !self.control & 63 != 0 {
            self.status |= 0x8080;
            // Precision is a post-computation exception: commit the rounded
            // result, memory store and stack pop before the next waiting
            // instruction reports the pending exception. Invalid/denormal/
            // zero-divide suppress completion; unmasked overflow/underflow
            // additionally need wrapped results (not yet implemented).
            self.status & !self.control & 31 == 0
        } else {
            true
        }
    }
    fn completed_result_exception(&mut self, bits: u16) -> bool {
        self.exception(bits)
            || (self.status & !self.control & 24 != 0 && self.status & !self.control & 7 == 0)
    }
    fn get(&self, index: usize, exceptions: &mut u16) -> u128 {
        let slot = (self.top + index) & 7;
        if self.tags[slot] != 3 {
            self.values[slot]
        } else {
            *exceptions |= 0x41;
            INDEFINITE
        }
    }
    fn set(&mut self, index: usize, bits: u128) {
        let slot = (self.top + index) & 7;
        self.values[slot] = bits;
        self.tags[slot] = value_tag(bits);
    }
    fn push(&mut self, mut value: u128, mut exceptions: u16) {
        self.status &= !0x200;
        let next = self.top.wrapping_sub(1) & 7;
        if self.tags[next] != 3 {
            exceptions |= 0x41;
            self.status |= 0x200;
            value = INDEFINITE;
        }
        if self.exception(exceptions) {
            self.top = next;
            self.set(0, value);
        }
    }
    fn pop(&mut self) {
        self.tags[self.top] = 3;
        self.top = (self.top + 1) & 7;
    }
}
fn from_float<F: Float + FloatConvert<Extended>>(bits: u128, exceptions: &mut u16) -> u128 {
    let value = F::from_bits(bits);
    if value.is_denormal() {
        *exceptions |= 2;
    }
    let result = value.convert_r(Round::NearestTiesToEven, &mut false);
    *exceptions |= flags(result.status);
    result.value.to_bits()
}
fn to_float<F: Float>(value: Extended, round: Round, exceptions: &mut u16, c1: &mut bool) -> u128
where
    Extended: FloatConvert<F>,
{
    let result: rustc_apfloat::StatusAnd<F> = value.convert_r(round, &mut false);
    *exceptions |= flags(result.status);
    if result.value.abs() == F::largest()
        && result.status.contains(Status::INEXACT)
        && value.abs() >= Extended::from_u128(1).value.scalbn(F::MAX_EXP + 1)
    {
        *exceptions |= 8;
    }
    if result.status.contains(Status::INEXACT) {
        let truncated: rustc_apfloat::StatusAnd<F> = value.convert_r(Round::TowardZero, &mut false);
        *c1 = result.value.abs() > truncated.value.abs();
    }
    result.value.to_bits()
}
impl X86 {
    fn x87_read(
        &self,
        memory: &Memory,
        source: Value,
        integer: bool,
        exceptions: &mut u16,
    ) -> Result<u128> {
        match source {
            Value::FloatStack(n) => Ok(self.fpu.get(n, exceptions)),
            Value::Memory { address, width } => {
                let mut bytes = [0; 16];
                memory.read(address, &mut bytes[..width], blink_core::Access::Read)?;
                let bits = u128::from_le_bytes(bytes);
                if integer {
                    Ok(Extended::from_i128(signed(bits as u64, width))
                        .value
                        .to_bits())
                } else {
                    Ok(match width {
                        4 => from_float::<Single>(bits, exceptions),
                        8 => from_float::<Double>(bits, exceptions),
                        10 => bits,
                        _ => {
                            return Err(Error::UnsupportedInstruction {
                                pc: self.rip,
                                encoding: 0,
                            })
                        }
                    })
                }
            }
            _ => Err(Error::UnsupportedInstruction {
                pc: self.rip,
                encoding: 0,
            }),
        }
    }
    pub(super) fn x87(
        &mut self,
        memory: &mut Memory,
        op: Op,
        a: Option<Value>,
        b: Option<Value>,
        pc: u64,
    ) -> Result<bool> {
        let bad = || Error::UnsupportedInstruction {
            pc,
            encoding: op as u32,
        };
        if !matches!(
            op,
            Op::FCMOVB
                | Op::FCMOVBE
                | Op::FCMOVE
                | Op::FCMOVNB
                | Op::FCMOVNBE
                | Op::FCMOVNE
                | Op::FCMOVNU
                | Op::FCMOVU
                | Op::FNINIT
                | Op::FNCLEX
                | Op::FNSTCW
                | Op::FNSTENV
                | Op::FLDENV
                | Op::FNSAVE
                | Op::FRSTOR
                | Op::FNSTSW
                | Op::FLDCW
                | Op::WAIT
                | Op::FNOP
                | Op::FLD
                | Op::FILD
                | Op::FBLD
                | Op::FBSTP
                | Op::FLD1
                | Op::FLDL2T
                | Op::FLDL2E
                | Op::FLDPI
                | Op::FLDLG2
                | Op::FLDLN2
                | Op::FXAM
                | Op::FLDZ
                | Op::FST
                | Op::FSTP
                | Op::FIST
                | Op::FISTP
                | Op::FISTTP
                | Op::FADD
                | Op::FADDP
                | Op::FSUB
                | Op::FSUBP
                | Op::FSUBR
                | Op::FSUBRP
                | Op::FMUL
                | Op::FMULP
                | Op::FDIV
                | Op::FDIVP
                | Op::FDIVR
                | Op::FDIVRP
                | Op::FIADD
                | Op::FISUB
                | Op::FISUBR
                | Op::FIMUL
                | Op::FIDIV
                | Op::FIDIVR
                | Op::FXCH
                | Op::FCHS
                | Op::FABS
                | Op::FFREE
                | Op::FFREEP
                | Op::FINCSTP
                | Op::FDECSTP
                | Op::FCOM
                | Op::FICOM
                | Op::FICOMP
                | Op::FCOMP
                | Op::FCOMPP
                | Op::FUCOM
                | Op::FUCOMP
                | Op::FUCOMPP
                | Op::FCOMI
                | Op::FCOMIP
                | Op::FUCOMI
                | Op::FUCOMIP
                | Op::FTST
                | Op::FRNDINT
                | Op::FSQRT
                | Op::FSCALE
                | Op::FPREM
                | Op::FPATAN
                | Op::F2XM1
                | Op::FYL2X
                | Op::FYL2XP1
                | Op::FSIN
                | Op::FCOS
                | Op::FSINCOS
                | Op::FPTAN
                | Op::FPREM1
                | Op::FXTRACT
        ) {
            return Ok(false);
        }
        if !matches!(
            op,
            Op::FNINIT | Op::FNCLEX | Op::FNSTCW | Op::FNSTSW | Op::FNSTENV | Op::FNSAVE
        ) && self.fpu.status & !self.fpu.control & 63 != 0
        {
            return Err(Error::ArithmeticFault);
        }
        if !matches!(
            op,
            Op::FNINIT
                | Op::FNCLEX
                | Op::FNSTCW
                | Op::FNSTSW
                | Op::FLDCW
                | Op::WAIT
                | Op::FNSTENV
                | Op::FLDENV
                | Op::FNSAVE
                | Op::FRSTOR
        ) {
            self.fpu.instruction = pc;
            for offset in 0..14 {
                let byte = memory.fetch(pc.wrapping_add(offset))?;
                if (0xd8..=0xdf).contains(&byte) {
                    self.fpu.opcode = ((byte as u16 & 7) << 8)
                        | memory.fetch(pc.wrapping_add(offset + 1))? as u16;
                    break;
                }
            }
            if let Some(Value::Memory { address, .. }) =
                a.filter(|v| matches!(v, Value::Memory { .. })).or(b)
            {
                self.fpu.data = address;
            }
        }
        match op {
            Op::FNSTENV | Op::FLDENV | Op::FNSAVE | Op::FRSTOR => {
                let Value::Memory { address, width } = a.ok_or_else(bad)? else {
                    return Err(bad());
                };
                let full = matches!(op, Op::FNSAVE | Op::FRSTOR);
                let environment = width - if full { 80 } else { 0 };
                if !matches!(environment, 14 | 28) {
                    return Err(bad());
                }
                let mut bytes = vec![0; width];
                if matches!(op, Op::FNSTENV | Op::FNSAVE) {
                    self.fpu.save_environment(&mut bytes[..environment]);
                    if full {
                        for n in 0..8 {
                            bytes[environment + n * 10..environment + (n + 1) * 10]
                                .copy_from_slice(
                                    &self.fpu.values[(self.fpu.top + n) & 7].to_le_bytes()[..10],
                                );
                        }
                    }
                    memory.write(address, &bytes)?;
                    if full {
                        self.fpu = Fpu {
                            values: self.fpu.values,
                            ..Fpu::default()
                        };
                    } else {
                        self.fpu.control |= 63;
                    }
                } else {
                    memory.read(address, &mut bytes, blink_core::Access::Read)?;
                    self.fpu.load_environment(&bytes[..environment]);
                    if full {
                        for n in 0..8 {
                            let mut value = [0; 16];
                            value[..10].copy_from_slice(
                                &bytes[environment + n * 10..environment + (n + 1) * 10],
                            );
                            self.fpu.values[(self.fpu.top + n) & 7] = u128::from_le_bytes(value);
                            let slot = (self.fpu.top + n) & 7;
                            if self.fpu.tags[slot] != 3 {
                                self.fpu.tags[slot] = value_tag(self.fpu.values[slot]);
                            }
                        }
                    }
                }
            }
            Op::FNINIT => {
                self.fpu = Fpu {
                    values: self.fpu.values,
                    ..Fpu::default()
                }
            }
            Op::FNCLEX => self.fpu.status &= !0x80ff,
            Op::FNSTCW => self.write(memory, a.ok_or_else(bad)?, self.fpu.control as u64)?,
            Op::FNSTSW => self.write(memory, a.ok_or_else(bad)?, self.fpu.status() as u64)?,
            Op::FLDCW => {
                self.fpu.control = self.read(memory, a.ok_or_else(bad)?)? as u16;
                self.fpu.status &= !0x8080;
                self.fpu.exception(0);
            }
            Op::WAIT | Op::FNOP => {}
            Op::FBLD => {
                let Value::Memory { address, width: 10 } = b.or(a).ok_or_else(bad)? else {
                    return Err(bad());
                };
                let mut bytes = [0; 10];
                memory.read(address, &mut bytes, blink_core::Access::Read)?;
                let mut number = 0u128;
                for byte in bytes[..9].iter().rev() {
                    if byte & 15 > 9 || byte >> 4 > 9 {
                        return Err(bad());
                    }
                    number = number * 100 + (byte >> 4) as u128 * 10 + (byte & 15) as u128;
                }
                let mut value = Extended::from_u128(number).value;
                if bytes[9] & 128 != 0 {
                    value = -value;
                }
                self.fpu.push(value.to_bits(), 0);
            }
            Op::FBSTP => {
                let Value::Memory { address, width: 10 } = a.ok_or_else(bad)? else {
                    return Err(bad());
                };
                memory.check(address, 10, blink_core::Access::Write)?;
                let mut exceptions = 0;
                let bits = self.fpu.get(0, &mut exceptions);
                let value = Extended::from_bits(bits);
                let integer = value.to_i128_r(64, self.fpu.round(), &mut false);
                let invalid = integer.status.contains(Status::INVALID_OP)
                    || integer.value.unsigned_abs() > 999_999_999_999_999_999;
                let mut bytes = [0u8; 10];
                self.fpu.status &= !0x200;
                if invalid {
                    exceptions |= 1;
                    bytes.copy_from_slice(&INDEFINITE.to_le_bytes()[..10]);
                } else {
                    exceptions |= flags(integer.status);
                    if Extended::from_i128(integer.value).value.abs() > value.abs() {
                        self.fpu.status |= 0x200;
                    }
                    let mut number = integer.value.unsigned_abs();
                    for byte in &mut bytes[..9] {
                        let low = number % 10;
                        number /= 10;
                        let high = number % 10;
                        number /= 10;
                        *byte = (low | (high << 4)) as u8;
                    }
                    bytes[9] = if value.is_negative() { 128 } else { 0 };
                }
                if self.fpu.exception(exceptions) {
                    memory.write(address, &bytes)?;
                    self.fpu.pop();
                }
            }
            Op::FLD | Op::FILD => {
                let mut exceptions = 0;
                let source = b.or(a).ok_or_else(bad)?;
                let value = self.x87_read(memory, source, op == Op::FILD, &mut exceptions)?;
                self.fpu.push(value, exceptions);
            }
            Op::FLDL2T | Op::FLDL2E | Op::FLDPI | Op::FLDLG2 | Op::FLDLN2 => {
                // Extended constants rounded by RC, independently of PC. The
                // architectural constant load never reports precision loss.
                let (down, nearest_up): (u128, bool) = match op {
                    Op::FLDL2T => (0x4000_d49a_784b_cd1b_8afe, false),
                    Op::FLDL2E => (0x3fff_b8aa_3b29_5c17_f0bb, true),
                    Op::FLDPI => (0x4000_c90f_daa2_2168_c234, true),
                    Op::FLDLG2 => (0x3ffd_9a20_9a84_fbcf_f798, true),
                    _ => (0x3ffe_b172_17f7_d1cf_79ab, true),
                };
                let up = match (self.fpu.control >> 10) & 3 {
                    0 => nearest_up,
                    2 => true,
                    _ => false,
                };
                self.fpu.push(down + u128::from(up), 0);
            }
            Op::FXAM => {
                let bits = self.fpu.values[self.fpu.top];
                let fraction = bits as u64;
                let exponent = (bits >> 64) & 0x7fff;
                let class = if self.fpu.tags[self.fpu.top] == 3 {
                    5
                } else if exponent == 0 {
                    if fraction == 0 {
                        4
                    } else {
                        6
                    }
                } else if fraction & (1 << 63) == 0 {
                    0
                } else if exponent == 0x7fff {
                    if fraction == 1 << 63 {
                        3
                    } else {
                        1
                    }
                } else {
                    2
                };
                self.fpu.status = (self.fpu.status & !0x4700)
                    | ((class & 1) << 8)
                    | ((class & 2) << 9)
                    | ((class & 4) << 12)
                    | (((bits >> 79) as u16 & 1) << 9);
            }
            Op::FLD1 | Op::FLDZ => self.fpu.push(
                if op == Op::FLD1 {
                    Extended::from_u128(1).value.to_bits()
                } else {
                    0
                },
                0,
            ),
            Op::FST | Op::FSTP | Op::FIST | Op::FISTP | Op::FISTTP => {
                let dst = a.ok_or_else(bad)?;
                let mut exceptions = 0;
                let bits = self.fpu.get(0, &mut exceptions);
                let value = if unsupported_extended(bits)
                    && !matches!(dst, Value::FloatStack(_) | Value::Memory { width: 10, .. })
                {
                    exceptions |= 1;
                    Extended::from_bits(INDEFINITE)
                } else {
                    Extended::from_bits(bits)
                };
                let mut c1 = false;
                let result = match dst {
                    Value::FloatStack(_) => bits,
                    Value::Memory { width, .. }
                        if matches!(op, Op::FIST | Op::FISTP | Op::FISTTP) =>
                    {
                        let round = if op == Op::FISTTP {
                            Round::TowardZero
                        } else {
                            self.fpu.round()
                        };
                        let result = value.to_i128_r(width * 8, round, &mut false);
                        exceptions |= flags(result.status);
                        if result.status.contains(Status::INVALID_OP) {
                            1u128 << (width * 8 - 1)
                        } else {
                            c1 = Extended::from_i128(result.value).value.abs() > value.abs();
                            result.value as u128
                        }
                    }
                    Value::Memory { width: 4, .. } => {
                        to_float::<Single>(value, self.fpu.round(), &mut exceptions, &mut c1)
                    }
                    Value::Memory { width: 8, .. } => {
                        to_float::<Double>(value, self.fpu.round(), &mut exceptions, &mut c1)
                    }
                    Value::Memory { width: 10, .. } => bits,
                    _ => return Err(bad()),
                };
                if let Value::Memory { address, width } = dst {
                    memory.check(address, width, blink_core::Access::Write)?;
                }
                self.fpu.status &= !0x200;
                if c1 {
                    self.fpu.status |= 0x200;
                }
                if self.fpu.exception(exceptions) {
                    match dst {
                        Value::FloatStack(n) => self.fpu.set(n, result),
                        Value::Memory { address, width } => {
                            memory.write(address, &result.to_le_bytes()[..width])?
                        }
                        _ => unreachable!(),
                    }
                    if matches!(op, Op::FSTP | Op::FISTP | Op::FISTTP) {
                        self.fpu.pop();
                    }
                }
            }
            Op::FCOM
            | Op::FICOM
            | Op::FICOMP
            | Op::FCOMP
            | Op::FCOMPP
            | Op::FUCOM
            | Op::FUCOMP
            | Op::FUCOMPP
            | Op::FCOMI
            | Op::FCOMIP
            | Op::FUCOMI
            | Op::FUCOMIP
            | Op::FTST => {
                let mut exceptions = 0;
                let left_bits = self.fpu.get(0, &mut exceptions);
                let left = arithmetic_input(left_bits, &mut exceptions);
                let right = if op == Op::FTST {
                    Extended::ZERO
                } else {
                    let bits = self.x87_read(
                        memory,
                        b.or(a).unwrap_or(Value::FloatStack(1)),
                        matches!(op, Op::FICOM | Op::FICOMP),
                        &mut exceptions,
                    )?;
                    arithmetic_input(bits, &mut exceptions)
                };
                let quiet = matches!(
                    op,
                    Op::FUCOM | Op::FUCOMP | Op::FUCOMPP | Op::FUCOMI | Op::FUCOMIP
                );
                let order = if left.is_nan() || right.is_nan() {
                    exceptions &= !2;
                    if !quiet || left.is_signaling() || right.is_signaling() {
                        exceptions |= 1;
                    }
                    None
                } else {
                    if left.is_denormal() || right.is_denormal() {
                        exceptions |= 2;
                    }
                    left.partial_cmp(&right)
                };
                self.fpu.status &= !0x200;
                if self.fpu.exception(exceptions) {
                    let code = match order {
                        None => 0x4500,
                        Some(std::cmp::Ordering::Less) => 0x100,
                        Some(std::cmp::Ordering::Equal) => 0x4000,
                        _ => 0,
                    };
                    if matches!(op, Op::FCOMI | Op::FCOMIP | Op::FUCOMI | Op::FUCOMIP) {
                        self.flags = (self.flags & !0x8d5) | (code as u64 >> 8);
                    } else {
                        self.fpu.status = (self.fpu.status & !0x4500) | code;
                    }
                    if matches!(
                        op,
                        Op::FCOMP
                            | Op::FICOMP
                            | Op::FCOMPP
                            | Op::FUCOMP
                            | Op::FUCOMPP
                            | Op::FCOMIP
                            | Op::FUCOMIP
                    ) {
                        self.fpu.pop();
                    }
                    if matches!(op, Op::FCOMPP | Op::FUCOMPP) {
                        self.fpu.pop();
                    }
                }
            }
            Op::FXTRACT => {
                let mut exceptions = 0;
                let bits = self.fpu.get(0, &mut exceptions);
                let value = Extended::from_bits(bits);
                let raw_exp = ((bits >> 64) & 0x7fff) as i32;
                let (mut significand, mut exponent) =
                    if exceptions != 0 || (raw_exp != 0 && bits & (1 << 63) == 0) {
                        exceptions |= 1;
                        (INDEFINITE, INDEFINITE)
                    } else if value.is_nan() {
                        if value.is_signaling() {
                            exceptions |= 1;
                        }
                        (bits | (1 << 62), bits | (1 << 62))
                    } else if value.is_zero() {
                        exceptions |= 4;
                        (bits, (-Extended::INFINITY).to_bits())
                    } else if value.is_infinite() {
                        (bits, Extended::INFINITY.to_bits())
                    } else {
                        let mut fraction = bits as u64;
                        let mut exponent = raw_exp.max(1) - 16383;
                        if raw_exp == 0 {
                            exceptions |= 2;
                        }
                        while fraction & (1 << 63) == 0 {
                            fraction <<= 1;
                            exponent -= 1;
                        }
                        (
                            fraction as u128 | (0x3fffu128 << 64) | (bits & (1 << 79)),
                            Extended::from_i128(exponent as i128).value.to_bits(),
                        )
                    };
                let next = self.fpu.top.wrapping_sub(1) & 7;
                self.fpu.status &= !0x200;
                if self.fpu.tags[next] != 3 {
                    exceptions |= 0x41;
                    self.fpu.status |= 0x200;
                    significand = INDEFINITE;
                    exponent = INDEFINITE;
                }
                if self.fpu.exception(exceptions) {
                    self.fpu.set(0, exponent);
                    self.fpu.top = next;
                    self.fpu.set(0, significand);
                }
            }
            Op::FSIN | Op::FCOS | Op::FSINCOS | Op::FPTAN => {
                let mut exceptions = 0;
                let bits = self.fpu.get(0, &mut exceptions);
                let value = arithmetic_input(bits, &mut exceptions);
                let push = matches!(op, Op::FSINCOS | Op::FPTAN);
                let (mut first, mut second, raised, up) = if unsupported_extended(bits)
                    || exceptions & 0x40 != 0
                    || value.is_infinite()
                {
                    (INDEFINITE, INDEFINITE, 1, false)
                } else if value.is_nan() {
                    let nan = bits | (1 << 62);
                    (nan, nan, u16::from(value.is_signaling()), false)
                } else if value.abs() >= Extended::from_u128(1).value.scalbn(63) {
                    self.fpu.status |= 0x400;
                    return Ok(true);
                } else {
                    let (sin, cos) = sin_cos(value);
                    let precision = !value.is_zero();
                    let (sin_bits, sin_flags, sin_up) =
                        transcend_result(sin, self.fpu.round(), precision, self.fpu.control);
                    let (cos_bits, cos_flags, cos_up) =
                        transcend_result(cos, self.fpu.round(), precision, self.fpu.control);
                    match op {
                        Op::FSIN => (sin_bits, 0, sin_flags, sin_up),
                        Op::FCOS => (cos_bits, 0, cos_flags, cos_up),
                        Op::FSINCOS => (cos_bits, sin_bits, sin_flags | cos_flags, cos_up),
                        _ => {
                            let (tan_bits, flags, up) = transcend_result(
                                (sin / cos).value,
                                self.fpu.round(),
                                precision,
                                self.fpu.control,
                            );
                            (Extended::from_u128(1).value.to_bits(), tan_bits, flags, up)
                        }
                    }
                };
                exceptions |= raised;
                if exceptions & 1 != 0 {
                    exceptions &= !2;
                }
                self.fpu.status &= !0x600;
                if up {
                    self.fpu.status |= 0x200;
                }
                let next = self.fpu.top.wrapping_sub(1) & 7;
                if push && self.fpu.tags[next] != 3 {
                    exceptions |= 0x41;
                    self.fpu.status |= 0x200;
                    first = INDEFINITE;
                    second = INDEFINITE;
                }
                if self.fpu.completed_result_exception(exceptions) {
                    if push {
                        self.fpu.set(0, second);
                        self.fpu.top = next;
                    }
                    self.fpu.set(0, first);
                }
            }
            Op::FYL2X | Op::FYL2XP1 => {
                let mut exceptions = 0;
                let xb = self.fpu.get(0, &mut exceptions);
                let yb = self.fpu.get(1, &mut exceptions);
                let x = arithmetic_input(xb, &mut exceptions);
                let y = arithmetic_input(yb, &mut exceptions);
                let plus_one = op == Op::FYL2XP1;
                let one = Extended::from_u128(1).value;
                let zero_argument = if plus_one { x == -one } else { x.is_zero() };
                let negative_argument = if plus_one {
                    x < -one
                } else {
                    x.is_negative() && !x.is_zero()
                };
                let zero_log = if plus_one { x.is_zero() } else { x == one };
                let (bits, raised, up) = if unsupported_extended(xb)
                    || unsupported_extended(yb)
                    || exceptions & 0x40 != 0
                {
                    (INDEFINITE, 1, false)
                } else if x.is_nan() || y.is_nan() {
                    let chosen = if !x.is_nan() {
                        yb
                    } else if !y.is_nan() {
                        xb
                    } else if x.is_signaling() != y.is_signaling() {
                        if x.is_signaling() {
                            yb
                        } else {
                            xb
                        }
                    } else if xb as u64 != yb as u64 {
                        if xb as u64 > yb as u64 {
                            xb
                        } else {
                            yb
                        }
                    } else {
                        xb.min(yb)
                    };
                    exceptions &= !2;
                    (
                        chosen | (1 << 62),
                        u16::from(x.is_signaling() || y.is_signaling()),
                        false,
                    )
                } else if negative_argument
                    || (y.is_zero() && (zero_argument || x.is_infinite()))
                    || (zero_log && y.is_infinite())
                {
                    (INDEFINITE, 1, false)
                } else if zero_argument {
                    (
                        Extended::INFINITY.copy_sign(-y).to_bits(),
                        if y.is_infinite() { 0 } else { 4 },
                        false,
                    )
                } else if x.is_infinite() {
                    (Extended::INFINITY.copy_sign(y).to_bits(), 0, false)
                } else {
                    let wx: Math = x.convert(&mut false).value;
                    let wy: Math = y.convert(&mut false).value;
                    let log = if plus_one {
                        log2_plus_one(wx)
                    } else {
                        log2_positive(wx)
                    };
                    if y.is_infinite() {
                        (
                            (if y.is_negative() != log.is_negative() {
                                -Extended::INFINITY
                            } else {
                                Extended::INFINITY
                            })
                            .to_bits(),
                            0,
                            false,
                        )
                    } else {
                        let value = (wy * log).value;
                        transcend_result(
                            value,
                            self.fpu.round(),
                            !y.is_zero() && !zero_log,
                            self.fpu.control,
                        )
                    }
                };
                exceptions |= raised;
                if exceptions & 5 != 0 {
                    exceptions &= !2;
                }
                self.fpu.status &= !0x200;
                if up {
                    self.fpu.status |= 0x200;
                }
                if self.fpu.completed_result_exception(exceptions) {
                    self.fpu.set(1, bits);
                    self.fpu.pop();
                }
            }
            Op::F2XM1 => {
                let mut exceptions = 0;
                let bits = self.fpu.get(0, &mut exceptions);
                let value = arithmetic_input(bits, &mut exceptions);
                let (result, raised, up) = if unsupported_extended(bits) || exceptions & 0x40 != 0 {
                    (INDEFINITE, 1, false)
                } else if value.is_nan() {
                    (bits | (1 << 62), u16::from(value.is_signaling()), false)
                } else if value.is_zero() {
                    (bits, 0, false)
                } else if value.is_infinite() {
                    (
                        if value.is_negative() {
                            Extended::from_i128(-1).value.to_bits()
                        } else {
                            bits
                        },
                        0,
                        false,
                    )
                } else if value.abs() > Extended::from_u128(1).value {
                    return Err(bad());
                } else {
                    exp2_minus_one(value, self.fpu.round(), self.fpu.control)
                };
                exceptions |= raised;
                if exceptions & 5 != 0 {
                    exceptions &= !2;
                }
                self.fpu.status &= !0x200;
                if up {
                    self.fpu.status |= 0x200;
                }
                if self.fpu.completed_result_exception(exceptions) {
                    self.fpu.set(0, result);
                }
            }
            Op::FPATAN => {
                let mut exceptions = 0;
                let xb = self.fpu.get(0, &mut exceptions);
                let yb = self.fpu.get(1, &mut exceptions);
                let x = arithmetic_input(xb, &mut exceptions);
                let y = arithmetic_input(yb, &mut exceptions);
                let (bits, raised, up) = if unsupported_extended(xb)
                    || unsupported_extended(yb)
                    || exceptions & 0x40 != 0
                {
                    (INDEFINITE, 1, false)
                } else if x.is_nan() || y.is_nan() {
                    let chosen = if !x.is_nan() {
                        yb
                    } else if !y.is_nan() {
                        xb
                    } else if x.is_signaling() != y.is_signaling() {
                        if x.is_signaling() {
                            yb
                        } else {
                            xb
                        }
                    } else if xb as u64 != yb as u64 {
                        if xb as u64 > yb as u64 {
                            xb
                        } else {
                            yb
                        }
                    } else {
                        xb.min(yb)
                    };
                    exceptions &= !2;
                    (
                        chosen | (1 << 62),
                        u16::from(x.is_signaling() || y.is_signaling()),
                        false,
                    )
                } else {
                    atan2(y, x, self.fpu.round(), self.fpu.control)
                };
                exceptions |= raised;
                if exceptions & 5 != 0 {
                    exceptions &= !2;
                }
                self.fpu.status &= !0x200;
                if up {
                    self.fpu.status |= 0x200;
                }
                if self.fpu.completed_result_exception(exceptions) {
                    self.fpu.set(1, bits);
                    self.fpu.pop();
                }
            }
            Op::FPREM | Op::FPREM1 => {
                let mut exceptions = 0;
                let left = self.fpu.get(0, &mut exceptions);
                let right = self.fpu.get(1, &mut exceptions);
                let x = Extended::from_bits(left);
                let y = Extended::from_bits(right);
                let unsupported =
                    |bits: u128| ((bits >> 64) & 0x7fff) != 0 && bits & (1 << 63) == 0;
                let mut condition = 0;
                let result = if exceptions != 0 || unsupported(left) || unsupported(right) {
                    exceptions |= 1;
                    INDEFINITE
                } else if x.is_nan() || y.is_nan() {
                    if x.is_signaling() || y.is_signaling() {
                        exceptions |= 1;
                    }
                    let chosen = if !x.is_nan() {
                        right
                    } else if !y.is_nan() {
                        left
                    } else if x.is_signaling() != y.is_signaling() {
                        if x.is_signaling() {
                            right
                        } else {
                            left
                        }
                    } else if left as u64 != right as u64 {
                        if left as u64 > right as u64 {
                            left
                        } else {
                            right
                        }
                    } else {
                        left.min(right)
                    };
                    chosen | (1 << 62)
                } else if x.is_infinite() || y.is_zero() {
                    exceptions = 1;
                    INDEFINITE
                } else {
                    if denormal_extended(left) || denormal_extended(right) {
                        exceptions |= 2;
                    }
                    if x.is_zero() || y.is_infinite() {
                        if denormal_extended(left) && left & (1 << 63) != 0 {
                            left | (1 << 64)
                        } else {
                            left
                        }
                    } else {
                        let normalize = |bits: u128| {
                            let mut fraction = bits as u64;
                            let mut exponent = (((bits >> 64) & 0x7fff) as i32).max(1) - 16383;
                            while fraction & (1 << 63) == 0 {
                                fraction <<= 1;
                                exponent -= 1;
                            }
                            (fraction as u128, exponent)
                        };
                        let (xs, xe) = normalize(left);
                        let (ys, ye) = normalize(right);
                        let difference = xe - ye;
                        if difference >= 64 {
                            // The ISA permits a reduction width from 32 to 63.
                            // N=32 keeps discarded quotient bits above bit 31.
                            condition = 0x400;
                            x.c_fmod(y.scalbn(difference - 32)).value.to_bits()
                        } else {
                            let mut quotient = 0;
                            if difference >= -1 {
                                let numerator = xs << difference.max(0);
                                let denominator = ys << (-difference).max(0);
                                quotient = numerator / denominator;
                                let remainder = numerator % denominator;
                                if op == Op::FPREM1
                                    && (remainder * 2 > denominator
                                        || (remainder * 2 == denominator && quotient & 1 != 0))
                                {
                                    quotient += 1;
                                }
                            }
                            condition = (((quotient & 4) << 6)
                                | ((quotient & 1) << 9)
                                | ((quotient & 2) << 13))
                                as u16;
                            if op == Op::FPREM1 {
                                x.ieee_rem(y).value.to_bits()
                            } else {
                                x.c_fmod(y).value.to_bits()
                            }
                        }
                    }
                };
                self.fpu.status = (self.fpu.status & !0x4700) | condition;
                if self.fpu.exception(exceptions) {
                    self.fpu.set(0, result);
                }
            }
            Op::FSCALE => {
                let mut exceptions = 0;
                let left = self.fpu.get(0, &mut exceptions);
                let right = self.fpu.get(1, &mut exceptions);
                let x = Extended::from_bits(left);
                let y = Extended::from_bits(right);
                let unsupported =
                    |bits: u128| ((bits >> 64) & 0x7fff) != 0 && bits & (1 << 63) == 0;
                let mut rounded_up = false;
                let result = if exceptions != 0 || unsupported(left) || unsupported(right) {
                    exceptions |= 1;
                    INDEFINITE
                } else if x.is_nan() || y.is_nan() {
                    if x.is_signaling() || y.is_signaling() {
                        exceptions |= 1;
                    }
                    let chosen = if !x.is_nan() {
                        right
                    } else if !y.is_nan() {
                        left
                    } else if x.is_signaling() != y.is_signaling() {
                        if x.is_signaling() {
                            right
                        } else {
                            left
                        }
                    } else if left as u64 != right as u64 {
                        if left as u64 > right as u64 {
                            left
                        } else {
                            right
                        }
                    } else {
                        left.min(right)
                    };
                    chosen | (1 << 62)
                } else {
                    if denormal_extended(left) || denormal_extended(right) {
                        exceptions |= 2;
                    }
                    if y.is_infinite() {
                        if (x.is_zero() && !y.is_negative()) || (x.is_infinite() && y.is_negative())
                        {
                            exceptions = 1;
                            INDEFINITE
                        } else if y.is_negative() {
                            Extended::ZERO.copy_sign(x).to_bits()
                        } else {
                            Extended::INFINITY.copy_sign(x).to_bits()
                        }
                    } else if x.is_zero() || x.is_infinite() {
                        left
                    } else {
                        let limit = Extended::from_u128(100000).value;
                        let exponent = if y.abs() > limit {
                            if y.is_negative() {
                                -100000
                            } else {
                                100000
                            }
                        } else {
                            y.to_i128_r(32, Round::TowardZero, &mut false).value as i32
                        };
                        let wide: IeeeFloat<Intermediate> = x.convert(&mut false).value;
                        let total_exponent = x.ilogb() + exponent;
                        let adjustment = if self.fpu.control & 8 == 0 && total_exponent >= 16384 {
                            exceptions |= 8;
                            -24576
                        } else if self.fpu.control & 16 == 0 && total_exponent < -16382 {
                            exceptions |= 16;
                            24576
                        } else {
                            0
                        };
                        // Once beyond the extended output range, further scaling
                        // cannot change its rounded overflow/underflow result.
                        // Bound the wider intermediate without losing trap scaling.
                        let adjusted_exponent = total_exponent + adjustment;
                        if adjustment != 0 && !(-16382..16384).contains(&adjusted_exponent) {
                            // A trap result that remains outside the normal
                            // exponent range saturates to infinity/zero, even
                            // under directed rounding, and raises precision.
                            exceptions |= 32;
                            rounded_up = adjusted_exponent >= 16384;
                            if rounded_up {
                                Extended::INFINITY
                            } else {
                                Extended::ZERO
                            }
                            .copy_sign(x)
                            .to_bits()
                        } else {
                            let target_exponent =
                                (total_exponent + adjustment).clamp(-20000, 20000);
                            let scaled = wide.scalbn(target_exponent - x.ilogb());
                            let converted: rustc_apfloat::StatusAnd<Extended> =
                                scaled.convert_r(self.fpu.round(), &mut false);
                            exceptions |= flags(converted.status);
                            // x87 detects tininess before rounding, including a
                            // subnormal exact value that rounds up to minimum normal.
                            if converted.status.contains(Status::INEXACT)
                                && scaled.abs()
                                    < IeeeFloat::<Intermediate>::from_u128(1).value.scalbn(-16382)
                            {
                                exceptions |= 16;
                            }
                            let widened: IeeeFloat<Intermediate> =
                                converted.value.convert(&mut false).value;
                            rounded_up = widened.abs() > scaled.abs();
                            if converted.value.abs() == Extended::largest()
                                && converted.status.contains(Status::INEXACT)
                                && scaled.abs()
                                    >= IeeeFloat::<Intermediate>::from_u128(1).value.scalbn(16384)
                            {
                                exceptions |= 8;
                            }
                            converted.value.to_bits()
                        }
                    }
                };
                self.fpu.status &= !0x200;
                if rounded_up {
                    self.fpu.status |= 0x200;
                }
                let complete = self.fpu.exception(exceptions);
                if complete
                    || (exceptions & !self.fpu.control & 24 != 0
                        && exceptions & !self.fpu.control & 7 == 0)
                {
                    self.fpu.set(0, result);
                }
            }
            Op::FSQRT => {
                let mut exceptions = 0;
                let bits = self.fpu.get(0, &mut exceptions);
                let (result, status, rounded_up) = square_root(bits, self.fpu.control);
                exceptions |= status;
                self.fpu.status &= !0x200;
                if rounded_up {
                    self.fpu.status |= 0x200;
                }
                if self.fpu.exception(exceptions) {
                    self.fpu.set(0, result);
                }
            }
            Op::FRNDINT => {
                let mut exceptions = 0;
                let bits = self.fpu.get(0, &mut exceptions);
                let value = arithmetic_input(bits, &mut exceptions);
                if value.is_denormal() {
                    exceptions |= 2;
                }
                let result = value.round_to_integral(self.fpu.round());
                exceptions |= flags(result.status);
                self.fpu.status &= !0x200;
                if result.value.abs() > value.abs() {
                    self.fpu.status |= 0x200;
                }
                if self.fpu.exception(exceptions) {
                    self.fpu.set(0, result.value.to_bits());
                }
            }
            Op::FCMOVB
            | Op::FCMOVBE
            | Op::FCMOVE
            | Op::FCMOVNB
            | Op::FCMOVNBE
            | Op::FCMOVNE
            | Op::FCMOVNU
            | Op::FCMOVU => {
                let Value::FloatStack(n) = b.ok_or_else(bad)? else {
                    return Err(bad());
                };
                let condition = match op {
                    Op::FCMOVB => self.flags & 1 != 0,
                    Op::FCMOVBE => self.flags & 65 != 0,
                    Op::FCMOVE => self.flags & 64 != 0,
                    Op::FCMOVNB => self.flags & 1 == 0,
                    Op::FCMOVNBE => self.flags & 65 == 0,
                    Op::FCMOVNE => self.flags & 64 == 0,
                    Op::FCMOVNU => self.flags & 4 == 0,
                    _ => self.flags & 4 != 0,
                };
                let mut exceptions = 0;
                self.fpu.get(0, &mut exceptions);
                let value = self.fpu.get(n, &mut exceptions);
                self.fpu.status &= !0x200;
                if self.fpu.exception(exceptions) {
                    if exceptions != 0 {
                        self.fpu.set(0, INDEFINITE);
                    } else if condition {
                        self.fpu.set(0, value);
                    }
                }
            }
            Op::FXCH => {
                let mut exceptions = 0;
                let Value::FloatStack(n) = b.or(a).ok_or_else(bad)? else {
                    return Err(bad());
                };
                let first = self.fpu.get(0, &mut exceptions);
                let second = self.fpu.get(n, &mut exceptions);
                self.fpu.status &= !0x200;
                if self.fpu.exception(exceptions) {
                    self.fpu.set(0, second);
                    self.fpu.set(n, first);
                }
            }
            Op::FCHS | Op::FABS => {
                let mut exceptions = 0;
                let bits = self.fpu.get(0, &mut exceptions);
                self.fpu.status &= !0x200;
                if self.fpu.exception(exceptions) {
                    self.fpu.set(
                        0,
                        if exceptions & 0x40 != 0 {
                            INDEFINITE
                        } else if op == Op::FCHS {
                            bits ^ (1 << 79)
                        } else {
                            bits & !(1 << 79)
                        },
                    );
                }
            }
            Op::FFREE | Op::FFREEP => {
                let Value::FloatStack(n) = a.ok_or_else(bad)? else {
                    return Err(bad());
                };
                self.fpu.tags[(self.fpu.top + n) & 7] = 3;
                if op == Op::FFREEP {
                    self.fpu.pop();
                }
            }
            Op::FINCSTP | Op::FDECSTP => {
                self.fpu.top =
                    self.fpu
                        .top
                        .wrapping_add(if op == Op::FINCSTP { 1 } else { usize::MAX })
                        & 7;
                self.fpu.status &= !0x200;
            }
            _ => {
                let dst = a.ok_or_else(bad)?;
                let src = b.ok_or_else(bad)?;
                let Value::FloatStack(n) = dst else {
                    return Err(bad());
                };
                let mut exceptions = 0;
                let left = self.x87_read(memory, dst, false, &mut exceptions)?;
                let right = self.x87_read(
                    memory,
                    src,
                    matches!(
                        op,
                        Op::FIADD | Op::FISUB | Op::FISUBR | Op::FIMUL | Op::FIDIV | Op::FIDIVR
                    ),
                    &mut exceptions,
                )?;
                let (left, right) = if matches!(
                    op,
                    Op::FSUBR | Op::FSUBRP | Op::FDIVR | Op::FDIVRP | Op::FISUBR | Op::FIDIVR
                ) {
                    (right, left)
                } else {
                    (left, right)
                };
                let x = arithmetic_input(left, &mut exceptions);
                let y = arithmetic_input(right, &mut exceptions);
                if x.is_denormal() || y.is_denormal() {
                    exceptions |= 2;
                }
                let (result, rounded_up) = if !x.is_finite()
                    || !y.is_finite()
                    || y.is_zero()
                        && matches!(
                            op,
                            Op::FDIV | Op::FDIVP | Op::FDIVR | Op::FDIVRP | Op::FIDIV | Op::FIDIVR
                        ) {
                    (calculate(op, x, y, self.fpu.round()), false)
                } else {
                    match (self.fpu.control >> 8) & 3 {
                        0 => reduced::<IeeeFloat<Precision24>>(op, x, y, self.fpu.round()),
                        2 => reduced::<IeeeFloat<Precision53>>(op, x, y, self.fpu.round()),
                        3 => {
                            let mut result = calculate(op, x, y, self.fpu.round());
                            if result.status.contains(Status::INEXACT)
                                && result.value.abs() == Extended::smallest_normalized()
                            {
                                let wx: IeeeFloat<Intermediate> = x.convert(&mut false).value;
                                let wy: IeeeFloat<Intermediate> = y.convert(&mut false).value;
                                if calculate(op, wx, wy, Round::TowardZero).value.abs()
                                    < IeeeFloat::<Intermediate>::from_u128(1).value.scalbn(-16382)
                                {
                                    result.status |= Status::UNDERFLOW;
                                }
                            }
                            let up = result.status.contains(Status::INEXACT)
                                && result.value.abs()
                                    > calculate(op, x, y, Round::TowardZero).value.abs();
                            (result, up)
                        }
                        _ => return Err(bad()),
                    }
                };
                exceptions |= flags(result.status);
                if x.is_nan() || y.is_nan() || exceptions & 5 != 0 {
                    exceptions &= !2;
                }
                if result.value.abs() == Extended::largest()
                    && result.status.contains(Status::INEXACT)
                {
                    let wx: IeeeFloat<WideExtended> = x.convert(&mut false).value;
                    let wy: IeeeFloat<WideExtended> = y.convert(&mut false).value;
                    let wide = calculate(op, wx, wy, self.fpu.round()).value;
                    if wide.abs()
                        >= IeeeFloat::<WideExtended>::from_u128(1)
                            .value
                            .scalbn(Extended::MAX_EXP + 1)
                    {
                        exceptions |= 8;
                    }
                }
                let mut bits = result.value.to_bits();
                if exceptions & 0x40 != 0
                    || unsupported_extended(left)
                    || unsupported_extended(right)
                {
                    bits = INDEFINITE;
                } else if x.is_nan() && y.is_nan() {
                    // x87 prefers a quiet NaN to a signaling NaN, then the
                    // larger significand, with positive sign breaking ties.
                    let chosen = if x.is_signaling() != y.is_signaling() {
                        if x.is_signaling() {
                            right
                        } else {
                            left
                        }
                    } else if (left as u64) != (right as u64) {
                        if (left as u64) > (right as u64) {
                            left
                        } else {
                            right
                        }
                    } else {
                        left.min(right)
                    };
                    bits = chosen | (1 << 62);
                } else if result.value.is_nan() && !x.is_nan() && !y.is_nan() {
                    bits = INDEFINITE;
                }
                let blocked_denormal = exceptions & !self.fpu.control & 2 != 0;
                if blocked_denormal {
                    exceptions &= !56;
                }
                let wrapped = if !blocked_denormal
                    && self.fpu.control & 24 != 24
                    && x.is_finite()
                    && y.is_finite()
                    && exceptions & 5 == 0
                {
                    match (self.fpu.control >> 8) & 3 {
                        0 => wrapped_arithmetic::<IeeeFloat<Trap24>>(
                            op,
                            x,
                            y,
                            self.fpu.control,
                            self.fpu.round(),
                        ),
                        2 => wrapped_arithmetic::<IeeeFloat<Trap53>>(
                            op,
                            x,
                            y,
                            self.fpu.control,
                            self.fpu.round(),
                        ),
                        3 => wrapped_arithmetic::<IeeeFloat<Trap64>>(
                            op,
                            x,
                            y,
                            self.fpu.control,
                            self.fpu.round(),
                        ),
                        _ => None,
                    }
                } else {
                    None
                };
                let mut rounded_up = rounded_up && !blocked_denormal;
                if let Some((value, raised, up)) = wrapped {
                    bits = value;
                    exceptions = (exceptions & !56) | raised;
                    rounded_up = up;
                }
                self.fpu.status &= !0x200;
                if rounded_up {
                    self.fpu.status |= 0x200;
                }
                let complete = self.fpu.exception(exceptions);
                if complete || (wrapped.is_some() && exceptions & !self.fpu.control & 7 == 0) {
                    self.fpu.set(n, bits);
                    if matches!(
                        op,
                        Op::FADDP | Op::FSUBP | Op::FSUBRP | Op::FMULP | Op::FDIVP | Op::FDIVRP
                    ) {
                        self.fpu.pop();
                    }
                }
            }
        }
        Ok(true)
    }
}

impl Fpu {
    fn save_environment(&self, bytes: &mut [u8]) {
        let tags = self
            .tags
            .iter()
            .enumerate()
            .fold(0u16, |word, (n, tag)| word | ((*tag as u16) << (n * 2)));
        let short = bytes.len() == 14;
        let stride = if short { 2 } else { 4 };
        bytes.fill(if short { 0 } else { 0xff });
        for (n, value) in [self.control, self.status(), tags].iter().enumerate() {
            bytes[n * stride..n * stride + 2].copy_from_slice(&value.to_le_bytes());
        }
        if short {
            bytes[6..8].copy_from_slice(&(self.instruction as u16).to_le_bytes());
            bytes[8..10].fill(0);
            bytes[10..12].copy_from_slice(&(self.data as u16).to_le_bytes());
            bytes[12..14].fill(0);
        } else {
            bytes[12..16].copy_from_slice(&(self.instruction as u32).to_le_bytes());
            bytes[16..18].fill(0);
            bytes[18..20].copy_from_slice(&self.opcode.to_le_bytes());
            bytes[20..24].copy_from_slice(&(self.data as u32).to_le_bytes());
            bytes[24..26].fill(0);
        }
    }
    fn load_environment(&mut self, bytes: &[u8]) {
        let word = |offset| u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
        let short = bytes.len() == 14;
        let stride = if short { 2 } else { 4 };
        self.control = word(0);
        self.status = word(stride);
        self.top = ((self.status >> 11) & 7) as usize;
        let tags = word(stride * 2);
        for n in 0..8 {
            self.tags[n] = if (tags >> (n * 2)) & 3 == 3 {
                3
            } else {
                value_tag(self.values[n])
            };
        }
        if short {
            self.instruction = word(6) as u64;
            self.data = word(10) as u64;
        } else {
            self.instruction = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as u64;
            self.opcode = word(18) & 0x7ff;
            self.data = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as u64;
        }
        // Loaded exception masks determine whether the next waiting instruction traps.
        self.status &= !0x8080;
        self.exception(0);
    }
    pub fn save_legacy(&self, bytes: &mut [u8]) {
        bytes[..2].copy_from_slice(&self.control.to_le_bytes());
        bytes[2..4].copy_from_slice(&self.status().to_le_bytes());
        bytes[6..8].copy_from_slice(&self.opcode.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.instruction.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.data.to_le_bytes());
        bytes[4] = self
            .tags
            .iter()
            .enumerate()
            .fold(0, |bits, (n, used)| bits | (u8::from(*used != 3) << n));
        for n in 0..8 {
            bytes[32 + n * 16..42 + n * 16]
                .copy_from_slice(&self.values[(self.top + n) & 7].to_le_bytes()[..10]);
        }
    }
    pub fn restore_legacy(bytes: &[u8]) -> Self {
        let control = u16::from_le_bytes(bytes[..2].try_into().unwrap());
        let status = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
        let top = ((status >> 11) & 7) as usize;
        let mut state = Self {
            control,
            status,
            top,
            opcode: u16::from_le_bytes(bytes[6..8].try_into().unwrap()) & 0x7ff,
            instruction: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            data: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            ..Self::default()
        };
        for n in 0..8 {
            let mut value = [0; 16];
            value[..10].copy_from_slice(&bytes[32 + n * 16..42 + n * 16]);
            state.values[(top + n) & 7] = u128::from_le_bytes(value);
            let slot = (top + n) & 7;
            state.tags[slot] = if bytes[4] & (1 << slot) != 0 {
                value_tag(state.values[slot])
            } else {
                3
            };
        }
        state
    }
}

impl X86 {
    pub(super) fn fxstate(
        &mut self,
        memory: &mut Memory,
        address: u64,
        restore: bool,
        wide: bool,
    ) -> Result<()> {
        let access = if restore {
            blink_core::Access::Read
        } else {
            blink_core::Access::Write
        };
        if address & 15 != 0 {
            return Err(Error::MemoryFault { address, access });
        }
        memory.check(address, 512, access)?;
        let mut bytes = [0; 512];
        if restore {
            memory.read(address, &mut bytes, access)?;
            let mxcsr = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
            if mxcsr & !0xffff != 0 {
                return Err(Error::MemoryFault { address, access });
            }
            if !wide {
                bytes[12..16].fill(0);
                bytes[20..24].fill(0);
            }
            let fpu = Fpu::restore_legacy(&bytes);
            for n in 0..16 {
                self.xmm[n].copy_from_slice(&bytes[160 + n * 16..176 + n * 16]);
            }
            self.mxcsr = mxcsr;
            self.fpu = fpu;
        } else {
            self.fpu.save_legacy(&mut bytes);
            if !wide {
                bytes[12..16].fill(0);
                bytes[20..24].fill(0);
            }
            bytes[24..28].copy_from_slice(&self.mxcsr.to_le_bytes());
            bytes[28..32].copy_from_slice(&0xffffu32.to_le_bytes());
            for n in 0..16 {
                bytes[160 + n * 16..176 + n * 16].copy_from_slice(&self.xmm[n]);
            }
            // The software-owned tail is not modified by FXSAVE.
            memory.write(address, &bytes[..416])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn machine(code: &[u8]) -> (X86, Memory) {
        let mut memory = Memory::new(8192);
        memory
            .map(0x1000, 4096, blink_core::Permissions::RX)
            .unwrap();
        memory
            .map(0x2000, 4096, blink_core::Permissions::RW)
            .unwrap();
        memory.initialize(0x1000, code).unwrap();
        (X86::new(0x1000, 0x3000), memory)
    }
    #[test]
    fn legacy_environment_faults_leave_state_and_destination_unchanged() {
        for (op, size) in [
            (Op::FNSTENV, 14),
            (Op::FNSTENV, 28),
            (Op::FNSAVE, 94),
            (Op::FNSAVE, 108),
            (Op::FLDENV, 14),
            (Op::FLDENV, 28),
            (Op::FRSTOR, 94),
            (Op::FRSTOR, 108),
        ] {
            let (mut cpu, mut memory) = machine(&[0xd9, 0xe8]);
            cpu.fpu.control = 0xb7f;
            cpu.fpu.push(Extended::from_u128(42).value.to_bits(), 0);
            cpu.fpu.instruction = 0x123456;
            let before = cpu.fpu.clone();
            memory.write(0x2ff0, &[0xa5; 16]).unwrap();
            assert!(cpu
                .x87(
                    &mut memory,
                    op,
                    Some(Value::Memory {
                        address: 0x2fff,
                        width: size
                    }),
                    None,
                    0x1000
                )
                .is_err());
            assert_eq!(cpu.fpu, before);
            let mut bytes = [0; 16];
            memory
                .read(0x2ff0, &mut bytes, blink_core::Access::Read)
                .unwrap();
            assert_eq!(bytes, [0xa5; 16]);
        }
    }
    #[test]
    fn remainder_reduction_reports_incomplete_and_preserves_unmasked_invalid() {
        for op in [Op::FPREM, Op::FPREM1] {
            let (mut cpu, mut memory) = machine(&[0xd9, 0xf8]);
            let x = Extended::from_bits(0x7ffe_fedc_ba98_7654_3210);
            let y = Extended::from_bits(3);
            cpu.fpu.set(0, x.to_bits());
            cpu.fpu.set(1, y.to_bits());
            cpu.x87(&mut memory, op, None, None, 0x1000).unwrap();
            assert_eq!(cpu.fpu.status() & 0x400, 0x400);
            let mut steps = 1;
            while cpu.fpu.status() & 0x400 != 0 {
                assert!(steps < 2048);
                cpu.x87(&mut memory, op, None, None, 0x1000).unwrap();
                steps += 1;
            }
            let expected = if op == Op::FPREM {
                x.c_fmod(y)
            } else {
                x.ieee_rem(y)
            };
            assert_eq!(cpu.fpu.get(0, &mut 0), expected.value.to_bits());
            assert_eq!(cpu.fpu.status() & 32, 0);
            cpu.fpu = Fpu::default();
            cpu.fpu.set(0, x.to_bits());
            cpu.fpu.set(1, 0);
            cpu.fpu.control &= !1;
            cpu.x87(&mut memory, op, None, None, 0x1000).unwrap();
            assert_eq!(cpu.fpu.get(0, &mut 0), x.to_bits());
            assert_eq!(cpu.fpu.status() & 0x8081, 0x8081);
        }
    }
    #[test]
    fn unmasked_transcendental_underflow_commits_both_stack_outputs() {
        for op in [Op::FSINCOS, Op::FPTAN] {
            let (mut cpu, mut memory) = machine(&[0x90]);
            cpu.fpu.set(0, 1); // smallest subnormal
            cpu.fpu.control &= !16;
            cpu.x87(&mut memory, op, None, None, 0x1000).unwrap();
            assert_eq!(cpu.fpu.top, 7);
            assert_eq!(
                cpu.fpu.get(0, &mut 0),
                Extended::from_u128(1).value.to_bits()
            );
            assert_eq!(
                cpu.fpu.get(1, &mut 0),
                Extended::from_u128(1).value.scalbn(8131).to_bits()
            );
            assert_eq!(cpu.fpu.status() & 0x80bf, 0x80b2);
            assert!(matches!(
                cpu.x87(&mut memory, Op::WAIT, None, None, 0x1000),
                Err(Error::ArithmeticFault)
            ));
        }
    }
    #[test]
    fn unmasked_overflow_commits_scaled_result_and_pop() {
        let (mut cpu, mut memory) = machine(&[0x90]);
        cpu.fpu.set(0, Extended::largest().to_bits());
        cpu.fpu.set(1, Extended::from_u128(2).value.to_bits());
        cpu.fpu.control &= !8;
        cpu.x87(
            &mut memory,
            Op::FMULP,
            Some(Value::FloatStack(1)),
            Some(Value::FloatStack(0)),
            0x1000,
        )
        .unwrap();
        assert_eq!(cpu.fpu.top, 1);
        assert_eq!(cpu.fpu.get(0, &mut 0), 0x1fff_ffff_ffff_ffff_ffff);
        assert_eq!(cpu.fpu.status() & 0x80bf, 0x8088);
        assert!(matches!(
            cpu.x87(&mut memory, Op::WAIT, None, None, 0x1000),
            Err(Error::ArithmeticFault)
        ));
    }
    #[test]
    fn unmasked_precision_commits_store_and_pop_before_deferred_fault() {
        let (mut cpu, mut memory) = machine(&[0xdb, 0x18]); // fistpl (%rax)
        cpu.regs[0] = 0x2000;
        cpu.fpu
            .set(0, Extended::from_u128(3).value.scalbn(-1).to_bits());
        cpu.fpu.control &= !32;
        cpu.step(&mut memory).unwrap();
        assert_eq!(memory.load(0x2000, 4).unwrap(), 2);
        assert_eq!(cpu.fpu.top, 1);
        assert_eq!(cpu.fpu.status() & 0x82a0, 0x82a0);
        assert!(matches!(
            cpu.x87(&mut memory, Op::WAIT, None, None, 0x1000),
            Err(Error::ArithmeticFault)
        ));
    }
    #[test]
    fn trig_range_rejection_preserves_registers_and_stack() {
        for op in [Op::FSIN, Op::FCOS, Op::FSINCOS, Op::FPTAN] {
            for sign in [1i128, -1] {
                let (mut cpu, mut memory) = machine(&[0x90]);
                cpu.fpu
                    .set(0, Extended::from_i128(sign).value.scalbn(63).to_bits());
                let before = cpu.fpu.clone();
                cpu.x87(&mut memory, op, None, None, 0x1000).unwrap();
                assert_eq!(cpu.fpu.values, before.values);
                assert_eq!(cpu.fpu.tags, before.tags);
                assert_eq!(cpu.fpu.top, before.top);
                assert_eq!(cpu.fpu.status() & 0x47f, 0x400);
            }
        }
    }
    #[test]
    fn trig_stack_overflow_invalid_and_zero_outputs() {
        for op in [Op::FSINCOS, Op::FPTAN] {
            let (mut cpu, mut memory) = machine(&[0x90]);
            cpu.fpu.set(0, 1u128 << 79); // negative zero
            cpu.x87(&mut memory, op, None, None, 0x1000).unwrap();
            assert_eq!(
                cpu.fpu.get(0, &mut 0),
                Extended::from_u128(1).value.to_bits()
            );
            assert_eq!(cpu.fpu.get(1, &mut 0), 1u128 << 79);
            assert_eq!(cpu.fpu.status() & 0x7f, 0);
            for masked in [true, false] {
                cpu.fpu = Fpu::default();
                for n in 0..8 {
                    cpu.fpu.push(Extended::from_u128(n + 1).value.to_bits(), 0);
                }
                if !masked {
                    cpu.fpu.control &= !1;
                }
                let before = cpu.fpu.clone();
                cpu.x87(&mut memory, op, None, None, 0x1000).unwrap();
                assert_eq!(cpu.fpu.status() & 0x241, 0x241);
                if masked {
                    assert_eq!(cpu.fpu.get(0, &mut 0), INDEFINITE);
                    assert_eq!(cpu.fpu.get(1, &mut 0), INDEFINITE);
                } else {
                    assert_eq!(cpu.fpu.values, before.values);
                    assert_eq!(cpu.fpu.tags, before.tags);
                    assert_eq!(cpu.fpu.top, before.top);
                }
            }
        }
    }
    #[test]
    fn atan_unmasked_invalid_does_not_pop_or_replace_destination() {
        let (mut cpu, mut memory) = machine(&[0xd9, 0xf3]);
        cpu.fpu.set(0, Extended::from_u128(1).value.to_bits());
        cpu.fpu.set(1, 1u128 | (1 << 64)); // unsupported unnormal encoding
        cpu.fpu.control &= !1;
        let before = cpu.fpu.clone();
        cpu.step(&mut memory).unwrap();
        assert_eq!(cpu.fpu.values, before.values);
        assert_eq!(cpu.fpu.tags, before.tags);
        assert_eq!(cpu.fpu.top, before.top);
        assert_eq!(cpu.fpu.status() & 0x8081, 0x8081);
    }
    #[test]
    fn extract_unmasked_stack_overflow_preserves_both_destinations() {
        let (mut cpu, mut memory) = machine(&[0xd9, 0xf4]);
        for n in 0..8 {
            cpu.fpu.push(Extended::from_u128(n + 1).value.to_bits(), 0);
        }
        cpu.fpu.control &= !1;
        let before = cpu.fpu.clone();
        cpu.step(&mut memory).unwrap();
        assert_eq!(cpu.fpu.values, before.values);
        assert_eq!(cpu.fpu.tags, before.tags);
        assert_eq!(cpu.fpu.top, before.top);
        assert_eq!(cpu.fpu.status() & 0x8241, 0x8241);
    }
    #[test]
    fn square_root_empty_stack_and_unmasked_invalid_preserve_state() {
        let (mut cpu, mut memory) = machine(&[0xd9, 0xfa]);
        cpu.step(&mut memory).unwrap();
        assert_eq!(cpu.fpu.get(0, &mut 0), INDEFINITE);
        assert_eq!(cpu.fpu.status() & 0x241, 0x41);
        cpu.rip = 0x1000;
        let negative = Extended::from_i128(-1).value.to_bits();
        cpu.fpu = Fpu::default();
        cpu.fpu.set(0, negative);
        cpu.fpu.control &= !1;
        cpu.step(&mut memory).unwrap();
        assert_eq!(cpu.fpu.get(0, &mut 0), negative);
        assert_eq!(cpu.fpu.status() & 0x8081, 0x8081);
        assert!(matches!(
            cpu.x87(&mut memory, Op::WAIT, None, None, 0x1000),
            Err(Error::ArithmeticFault)
        ));
    }
    #[test]
    fn extended_load_at_page_end_and_stack_overflow() {
        let (mut cpu, mut memory) = machine(&[0xdb, 0x28]); // fldt (%rax)
        cpu.regs[0] = 0x2ff6;
        let bits = Extended::from_u128(123).value.to_bits();
        memory
            .write(cpu.regs[0], &bits.to_le_bytes()[..10])
            .unwrap();
        cpu.step(&mut memory).unwrap();
        assert_eq!(cpu.fpu.get(0, &mut 0), bits);
        for _ in 0..8 {
            cpu.x87(&mut memory, Op::FLD1, None, None, 0x1000).unwrap();
        }
        assert_eq!(cpu.fpu.get(0, &mut 0), INDEFINITE);
        assert_eq!(cpu.fpu.status() & 0x241, 0x241);
    }
    #[test]
    fn unmasked_exception_is_deferred_and_preserves_stack() {
        let (mut cpu, mut memory) = machine(&[0xd9, 0xe8]);
        cpu.fpu.set(0, 0);
        cpu.fpu.set(1, Extended::from_u128(1).value.to_bits());
        cpu.fpu.control &= !4;
        cpu.x87(
            &mut memory,
            Op::FDIVP,
            Some(Value::FloatStack(1)),
            Some(Value::FloatStack(0)),
            0x1000,
        )
        .unwrap();
        assert_eq!(cpu.fpu.top, 0);
        assert_eq!(
            cpu.fpu.get(1, &mut 0),
            Extended::from_u128(1).value.to_bits()
        );
        assert_eq!(cpu.fpu.status() & 0x8084, 0x8084);
        cpu.x87(
            &mut memory,
            Op::FNSTSW,
            Some(Value::Memory {
                address: 0x2000,
                width: 2,
            }),
            None,
            0x1000,
        )
        .unwrap();
        assert_eq!(memory.load(0x2000, 2).unwrap() & 0x8084, 0x8084);
        assert!(matches!(
            cpu.x87(&mut memory, Op::WAIT, None, None, 0x1000),
            Err(Error::ArithmeticFault)
        ));
        cpu.x87(&mut memory, Op::FNCLEX, None, None, 0x1000)
            .unwrap();
        cpu.x87(&mut memory, Op::WAIT, None, None, 0x1000).unwrap();
    }
    #[test]
    fn signal_handler_starts_clean_and_return_restores_x87_stack() {
        let (mut cpu, mut memory) = machine(&[0xd9, 0xe8]);
        cpu.fpu.control = 0x77f;
        cpu.fpu.push(Extended::from_i128(-123).value.to_bits(), 0);
        cpu.fpu.push(Extended::from_u128(456).value.to_bits(), 0);
        let saved = cpu.fpu.clone();
        cpu.enter_signal(&mut memory, 10, 0x1010, 0x1020, 0, 1)
            .unwrap();
        assert_eq!(cpu.fpu.control, 0x37f);
        assert_eq!(cpu.fpu.tags, [3; 8]);
        cpu.fpu.push(Extended::from_u128(999).value.to_bits(), 0);
        cpu.regs[4] += 8;
        cpu.restore_signal(&memory).unwrap();
        assert_eq!(cpu.fpu.control, saved.control);
        assert_eq!(cpu.fpu.status(), saved.status());
        assert_eq!(cpu.fpu.values, saved.values);
        assert_eq!(cpu.fpu.tags, saved.tags);
    }
}
