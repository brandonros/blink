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
        if matches!(op, Op::MOVMSKPS | Op::MOVMSKPD | Op::PEXTRW) {
            let source = self.vector(memory, b.ok_or_else(bad)?)?;
            let result = if op == Op::PEXTRW {
                let index = (self.read(memory, c.ok_or_else(bad)?)? as usize)
                    & (b.ok_or_else(bad)?.width() / 2 - 1);
                u16::from_le_bytes(source[index * 2..index * 2 + 2].try_into().unwrap()) as u64
            } else {
                let width = if op == Op::MOVMSKPS { 4 } else { 8 };
                (0..16 / width).fold(0, |bits, n| {
                    bits | ((source[(n + 1) * width - 1] >> 7) as u64) << n
                })
            };
            self.write(memory, a.ok_or_else(bad)?, result)?;
            return Ok(true);
        }
        if op == Op::PINSRW {
            let dst = a.ok_or_else(bad)?;
            let mut result = self.vector(memory, dst)?;
            let value = self.read(memory, b.ok_or_else(bad)?)? as u16;
            let index = (self.read(memory, c.ok_or_else(bad)?)? as usize) & (dst.width() / 2 - 1);
            result[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
            self.write_vector(memory, dst, result)?;
            return Ok(true);
        }
        if !matches!(
            op,
            Op::PMINSW
                | Op::PMAXSW
                | Op::UNPCKLPS
                | Op::UNPCKHPS
                | Op::UNPCKLPD
                | Op::UNPCKHPD
                | Op::PMULLW
                | Op::PMULHW
                | Op::PMULHUW
                | Op::PMULUDQ
                | Op::PMADDWD
                | Op::PSADBW
                | Op::PAVGB
                | Op::PAVGW
                | Op::PADDSB
                | Op::PADDSW
                | Op::PADDUSB
                | Op::PADDUSW
                | Op::PSUBSB
                | Op::PSUBSW
                | Op::PSUBUSB
                | Op::PSUBUSW
                | Op::PSLLW
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
            Op::PMINSW | Op::PMAXSW => {
                for offset in (0..16).step_by(2) {
                    let l = i16::from_le_bytes(left[offset..offset + 2].try_into().unwrap());
                    let r = i16::from_le_bytes(right[offset..offset + 2].try_into().unwrap());
                    let value = if op == Op::PMINSW { l.min(r) } else { l.max(r) };
                    out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
                }
            }
            Op::UNPCKLPS | Op::UNPCKHPS | Op::UNPCKLPD | Op::UNPCKHPD => {
                let width = if matches!(op, Op::UNPCKLPS | Op::UNPCKHPS) {
                    4
                } else {
                    8
                };
                let start = if matches!(op, Op::UNPCKHPS | Op::UNPCKHPD) {
                    8
                } else {
                    0
                };
                for n in 0..8 / width {
                    out[n * width * 2..n * width * 2 + width]
                        .copy_from_slice(&left[start + n * width..start + (n + 1) * width]);
                    out[n * width * 2 + width..(n + 1) * width * 2]
                        .copy_from_slice(&right[start + n * width..start + (n + 1) * width]);
                }
            }
            Op::PMULLW | Op::PMULHW | Op::PMULHUW => {
                for offset in (0..16).step_by(2) {
                    let l = u16::from_le_bytes(left[offset..offset + 2].try_into().unwrap());
                    let r = u16::from_le_bytes(right[offset..offset + 2].try_into().unwrap());
                    let result = match op {
                        Op::PMULLW => l.wrapping_mul(r),
                        Op::PMULHW => (((l as i16 as i32) * (r as i16 as i32)) >> 16) as u16,
                        _ => ((l as u32 * r as u32) >> 16) as u16,
                    };
                    out[offset..offset + 2].copy_from_slice(&result.to_le_bytes());
                }
            }
            Op::PMULUDQ => {
                for offset in [0, 8] {
                    let l = u32::from_le_bytes(left[offset..offset + 4].try_into().unwrap()) as u64;
                    let r =
                        u32::from_le_bytes(right[offset..offset + 4].try_into().unwrap()) as u64;
                    out[offset..offset + 8].copy_from_slice(&(l * r).to_le_bytes());
                }
            }
            Op::PMADDWD => {
                for offset in (0..16).step_by(4) {
                    let mut result = 0i32;
                    for lane in [offset, offset + 2] {
                        let l = i16::from_le_bytes(left[lane..lane + 2].try_into().unwrap()) as i32;
                        let r =
                            i16::from_le_bytes(right[lane..lane + 2].try_into().unwrap()) as i32;
                        result = result.wrapping_add(l * r);
                    }
                    out[offset..offset + 4].copy_from_slice(&result.to_le_bytes());
                }
            }
            Op::PSADBW => {
                for offset in [0, 8] {
                    let total: u64 = (offset..offset + 8)
                        .map(|n| left[n].abs_diff(right[n]) as u64)
                        .sum();
                    out[offset..offset + 8].copy_from_slice(&total.to_le_bytes());
                }
            }
            Op::PAVGB
            | Op::PAVGW
            | Op::PADDSB
            | Op::PADDSW
            | Op::PADDUSB
            | Op::PADDUSW
            | Op::PSUBSB
            | Op::PSUBSW
            | Op::PSUBUSB
            | Op::PSUBUSW => {
                let width = if matches!(
                    op,
                    Op::PAVGB | Op::PADDSB | Op::PADDUSB | Op::PSUBSB | Op::PSUBUSB
                ) {
                    1
                } else {
                    2
                };
                let signed_lanes = matches!(op, Op::PADDSB | Op::PADDSW | Op::PSUBSB | Op::PSUBSW);
                let subtract = matches!(op, Op::PSUBSB | Op::PSUBSW | Op::PSUBUSB | Op::PSUBUSW);
                for offset in (0..16).step_by(width) {
                    let lane = |source: &[u8; 16]| {
                        let mut bytes = [0; 8];
                        bytes[..width].copy_from_slice(&source[offset..offset + width]);
                        let value = u64::from_le_bytes(bytes);
                        if signed_lanes {
                            signed(value, width)
                        } else {
                            value as i128
                        }
                    };
                    let l = lane(&left);
                    let r = lane(&right);
                    let result = if matches!(op, Op::PAVGB | Op::PAVGW) {
                        (l + r + 1) / 2
                    } else {
                        let (min, max) = if signed_lanes {
                            (-(1i128 << (width * 8 - 1)), (1i128 << (width * 8 - 1)) - 1)
                        } else {
                            (0, (1i128 << (width * 8)) - 1)
                        };
                        (if subtract { l - r } else { l + r }).clamp(min, max)
                    } as u64;
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
                    for n in 0..dst.width() / width {
                        let mut bytes = [0; 8];
                        bytes[..width].copy_from_slice(&source[n * width..(n + 1) * width]);
                        let value =
                            signed(u64::from_le_bytes(bytes), width).clamp(minimum, maximum) as u64;
                        let offset = half * (dst.width() / 2) + n * output_width;
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
