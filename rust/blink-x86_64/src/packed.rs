//! Additional SSE/SSE2 packed integer and lane-selection operations.
use super::*;
impl X86 {
    pub(super) fn packed(
        &mut self,
        memory: &mut Memory,
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
        if !matches!(
            op,
            Op::PSLLW
                | Op::PSLLD
                | Op::PSRLW
                | Op::PSRLD
                | Op::PSRAW
                | Op::PSRAD
                | Op::SHUFPS
                | Op::SHUFPD
                | Op::PSHUFLW
                | Op::PSHUFHW
                | Op::PACKSSWB
                | Op::PACKSSDW
                | Op::PACKUSWB
                | Op::ANDPS
                | Op::ANDPD
                | Op::ANDNPS
                | Op::ANDNPD
                | Op::ORPS
                | Op::ORPD
        ) {
            return Ok(false);
        }
        let dst = a.ok_or_else(bad)?;
        let left = self.vector(memory, dst)?;
        let right = self.vector(memory, b.ok_or_else(bad)?)?;
        let mut out = [0; 16];
        match op {
            Op::ANDPS | Op::ANDPD | Op::ANDNPS | Op::ANDNPD | Op::ORPS | Op::ORPD => {
                for n in 0..16 {
                    out[n] = match op {
                        Op::ANDPS | Op::ANDPD => left[n] & right[n],
                        Op::ANDNPS | Op::ANDNPD => !left[n] & right[n],
                        _ => left[n] | right[n],
                    };
                }
            }
            Op::PSLLW | Op::PSLLD | Op::PSRLW | Op::PSRLD | Op::PSRAW | Op::PSRAD => {
                let width = if matches!(op, Op::PSLLW | Op::PSRLW | Op::PSRAW) {
                    2
                } else {
                    4
                };
                let count = u64::from_le_bytes(right[..8].try_into().unwrap());
                for offset in (0..16).step_by(width) {
                    let mut bytes = [0; 8];
                    bytes[..width].copy_from_slice(&left[offset..offset + width]);
                    let value = u64::from_le_bytes(bytes);
                    let result = if matches!(op, Op::PSRAW | Op::PSRAD) {
                        (signed(value, width) >> count.min(width as u64 * 8 - 1)) as u64
                    } else if count >= width as u64 * 8 {
                        0
                    } else if matches!(op, Op::PSLLW | Op::PSLLD) {
                        value << count
                    } else {
                        value >> count
                    };
                    out[offset..offset + width].copy_from_slice(&result.to_le_bytes()[..width]);
                }
            }
            Op::SHUFPS | Op::SHUFPD => {
                let imm = self.read(memory, c.ok_or_else(bad)?)?;
                let width = if op == Op::SHUFPS { 4 } else { 8 };
                let bits = if width == 4 { 2 } else { 1 };
                for n in 0..16 / width {
                    let source = if n < 8 / width { &left } else { &right };
                    let index = ((imm >> (n * bits)) & ((1 << bits) - 1)) as usize;
                    out[n * width..(n + 1) * width]
                        .copy_from_slice(&source[index * width..(index + 1) * width]);
                }
            }
            Op::PSHUFLW | Op::PSHUFHW => {
                let imm = self.read(memory, c.ok_or_else(bad)?)?;
                out = right;
                let start = if op == Op::PSHUFLW { 0 } else { 8 };
                for n in 0..4 {
                    let index = ((imm >> (n * 2)) & 3) as usize;
                    out[start + n * 2..start + n * 2 + 2]
                        .copy_from_slice(&right[start + index * 2..start + index * 2 + 2]);
                }
            }
            _ => {
                let width = if op == Op::PACKSSDW { 4 } else { 2 };
                let output_width = width / 2;
                let (minimum, maximum) = if op == Op::PACKUSWB {
                    (0, 255)
                } else if output_width == 1 {
                    (-128, 127)
                } else {
                    (-32768, 32767)
                };
                for (half, source) in [&left, &right].iter().enumerate() {
                    for n in 0..16 / width {
                        let mut bytes = [0; 8];
                        bytes[..width].copy_from_slice(&source[n * width..(n + 1) * width]);
                        let value =
                            signed(u64::from_le_bytes(bytes), width).clamp(minimum, maximum) as u64;
                        let offset = half * 8 + n * output_width;
                        out[offset..offset + output_width]
                            .copy_from_slice(&value.to_le_bytes()[..output_width]);
                    }
                }
            }
        }
        self.write_vector(memory, dst, out)?;
        Ok(true)
    }
}
