//! Bounded writes to caller-owned output sinks. Guest status never changes host fd flags.
use super::*;

pub(super) fn transfer(
    memory: &Memory,
    sink: &mut dyn Write,
    address: u64,
    count: usize,
) -> Result<Option<u64>> {
    let count = count.min(65536);
    if memory.check(address, count, Access::Read).is_err() {
        return Ok(Some(errno(14)));
    }
    let mut done = 0;
    let mut bytes = [0; 4096];
    while done < count {
        let size = (count - done).min(bytes.len());
        memory.read(address + done as u64, &mut bytes[..size], Access::Read)?;
        match sink.write(&bytes[..size]) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into()),
            Ok(wrote) => {
                done += wrote;
                if wrote < size {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(if done == 0 { None } else { Some(done as u64) });
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(done as u64))
}

pub(super) fn vectored(
    memory: &Memory,
    sink: &mut dyn Write,
    address: u64,
    count: u64,
) -> Result<Option<u64>> {
    if count > 1024 {
        return Ok(Some(errno(22)));
    }
    if memory
        .check(address, count as usize * 16, Access::Read)
        .is_err()
    {
        return Ok(Some(errno(14)));
    }
    let mut vectors = Vec::with_capacity(count as usize);
    let mut total = 0;
    for index in 0..count {
        let base = memory.load(address + index * 16, 8)?;
        let length = memory.load(address + index * 16 + 8, 8)?;
        if length > 0x7ffff000 || total + length > 0x7ffff000 {
            return Ok(Some(errno(22)));
        }
        total += length;
        vectors.push((base, length));
    }
    let mut done = 0;
    for (base, length) in vectors {
        let take = length.min(65536 - done) as usize;
        match transfer(memory, sink, base, take)? {
            None => return Ok(if done == 0 { None } else { Some(done) }),
            Some(value) if (value as i64) < 0 => {
                return Ok(Some(if done == 0 { value } else { done }))
            }
            Some(value) => {
                done += value;
                if value < length || done == 65536 {
                    break;
                }
            }
        }
    }
    Ok(Some(done))
}
