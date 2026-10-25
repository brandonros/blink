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
const INDEFINITE: u128 = 0xffff_c000_0000_0000_0000;
#[derive(Clone)]
pub(crate) struct Fpu {
    pub values: [u128; 8],
    pub occupied: [bool; 8],
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
            occupied: [false; 8],
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
    fn exception(&mut self, bits: u16) -> bool {
        self.status |= bits;
        if self.status & !self.control & 63 != 0 {
            self.status |= 0x8080;
            false
        } else {
            true
        }
    }
    fn get(&self, index: usize, exceptions: &mut u16) -> u128 {
        let slot = (self.top + index) & 7;
        if self.occupied[slot] {
            self.values[slot]
        } else {
            *exceptions |= 0x41;
            INDEFINITE
        }
    }
    fn set(&mut self, index: usize, bits: u128) {
        let slot = (self.top + index) & 7;
        self.values[slot] = bits;
        self.occupied[slot] = true;
    }
    fn push(&mut self, mut value: u128, mut exceptions: u16) {
        self.status &= !0x200;
        let next = self.top.wrapping_sub(1) & 7;
        if self.occupied[next] {
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
        self.occupied[self.top] = false;
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
            Op::FNINIT
                | Op::FNCLEX
                | Op::FNSTCW
                | Op::FNSTSW
                | Op::FLDCW
                | Op::WAIT
                | Op::FNOP
                | Op::FLD
                | Op::FILD
                | Op::FLD1
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
        ) {
            return Ok(false);
        }
        if !matches!(op, Op::FNINIT | Op::FNCLEX | Op::FNSTCW | Op::FNSTSW)
            && self.fpu.status & !self.fpu.control & 63 != 0
        {
            return Err(Error::ArithmeticFault);
        }
        if !matches!(
            op,
            Op::FNINIT | Op::FNCLEX | Op::FNSTCW | Op::FNSTSW | Op::FLDCW | Op::WAIT
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
            Op::FNINIT => self.fpu = Fpu::default(),
            Op::FNCLEX => self.fpu.status &= !0x80ff,
            Op::FNSTCW => self.write(memory, a.ok_or_else(bad)?, self.fpu.control as u64)?,
            Op::FNSTSW => self.write(memory, a.ok_or_else(bad)?, self.fpu.status() as u64)?,
            Op::FLDCW => {
                self.fpu.control = self.read(memory, a.ok_or_else(bad)?)? as u16;
                self.fpu.status &= !0x8080;
                self.fpu.exception(0);
            }
            Op::WAIT | Op::FNOP => {}
            Op::FLD | Op::FILD => {
                let mut exceptions = 0;
                let source = b.or(a).ok_or_else(bad)?;
                let value = self.x87_read(memory, source, op == Op::FILD, &mut exceptions)?;
                self.fpu.push(value, exceptions);
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
                let value = Extended::from_bits(bits);
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
                let left = Extended::from_bits(self.fpu.get(0, &mut exceptions));
                let right = if op == Op::FTST {
                    Extended::ZERO
                } else {
                    Extended::from_bits(self.x87_read(
                        memory,
                        b.or(a).unwrap_or(Value::FloatStack(1)),
                        false,
                        &mut exceptions,
                    )?)
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
            Op::FRNDINT => {
                let mut exceptions = 0;
                let value = Extended::from_bits(self.fpu.get(0, &mut exceptions));
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
                        if op == Op::FCHS {
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
                self.fpu.occupied[(self.fpu.top + n) & 7] = false;
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
                let x = Extended::from_bits(left);
                let y = Extended::from_bits(right);
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
                            let result = calculate(op, x, y, self.fpu.round());
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
                if x.is_nan() && y.is_nan() {
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
                self.fpu.status &= !0x200;
                if result.status.contains(Status::INEXACT) && rounded_up {
                    self.fpu.status |= 0x200;
                }
                if self.fpu.exception(exceptions) {
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
    pub fn save_legacy(&self, bytes: &mut [u8]) {
        bytes[..2].copy_from_slice(&self.control.to_le_bytes());
        bytes[2..4].copy_from_slice(&self.status().to_le_bytes());
        bytes[6..8].copy_from_slice(&self.opcode.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.instruction.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.data.to_le_bytes());
        bytes[4] = self
            .occupied
            .iter()
            .enumerate()
            .fold(0, |bits, (n, used)| bits | (u8::from(*used) << n));
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
            state.occupied[n] = bytes[4] & (1 << n) != 0;
            let mut value = [0; 16];
            value[..10].copy_from_slice(&bytes[32 + n * 16..42 + n * 16]);
            state.values[(top + n) & 7] = u128::from_le_bytes(value);
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
        assert_eq!(cpu.fpu.occupied, [false; 8]);
        cpu.fpu.push(Extended::from_u128(999).value.to_bits(), 0);
        cpu.regs[4] += 8;
        cpu.restore_signal(&memory).unwrap();
        assert_eq!(cpu.fpu.control, saved.control);
        assert_eq!(cpu.fpu.status(), saved.status());
        assert_eq!(cpu.fpu.values, saved.values);
        assert_eq!(cpu.fpu.occupied, saved.occupied);
    }
}
