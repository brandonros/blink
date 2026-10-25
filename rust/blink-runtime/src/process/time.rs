//! Guest-owned real timers and cooperative sleeps; never host signal timers.
use super::*;
use std::time::Duration;
#[derive(Default)]
pub(super) struct RealTimer {
    deadline: Option<Instant>,
    interval: Duration,
}
impl RealTimer {
    pub(super) fn expired(&mut self) -> bool {
        let now = Instant::now();
        let Some(deadline) = self.deadline.filter(|d| *d <= now) else {
            return false;
        };
        self.deadline = if self.interval.is_zero() {
            None
        } else {
            let remainder = now.duration_since(deadline).as_nanos() % self.interval.as_nanos();
            let remainder = Duration::new(
                (remainder / 1_000_000_000) as u64,
                (remainder % 1_000_000_000) as u32,
            );
            now.checked_add(self.interval - remainder)
        };
        true
    }
    fn remaining(&self) -> Duration {
        self.deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    }
}
fn duration(
    memory: &Memory,
    address: u64,
    microseconds: bool,
) -> std::result::Result<Duration, u64> {
    if memory.check(address, 16, Access::Read).is_err() {
        return Err(errno(14));
    }
    let seconds = memory.load(address, 8).map_err(|_| errno(14))?;
    let part = memory.load(address + 8, 8).map_err(|_| errno(14))?;
    let units = if microseconds {
        1_000_000
    } else {
        1_000_000_000
    };
    if seconds > i64::MAX as u64 || part >= units {
        return Err(errno(22));
    }
    Ok(Duration::new(
        seconds,
        (part * if microseconds { 1000 } else { 1 }) as u32,
    ))
}
fn timeval(bytes: &mut [u8], value: Duration) {
    let rounded = value.as_micros() + u128::from(value.subsec_nanos() % 1000 != 0);
    bytes[..8].copy_from_slice(&((rounded / 1_000_000) as u64).to_le_bytes());
    bytes[8..16].copy_from_slice(&((rounded % 1_000_000) as u64).to_le_bytes());
}
pub(super) fn interval(process: &Process, nr: u64, args: [u64; 6]) -> Result<u64> {
    let mut timer = process.real_timer.borrow_mut();
    if nr == 37 {
        let value = Duration::from_secs(args[0] as u32 as u64);
        let deadline = if value.is_zero() {
            None
        } else {
            Some(
                Instant::now()
                    .checked_add(value)
                    .ok_or(Error::ResourceLimit)?,
            )
        };
        let old = timer.remaining();
        timer.deadline = deadline;
        timer.interval = Duration::ZERO;
        return Ok(old.as_secs() + u64::from(old.subsec_nanos() != 0));
    }
    if args[0] != 0 {
        return Err(Error::UnsupportedSyscall(nr));
    }
    let mut memory = process.memory.borrow_mut();
    let old_pointer = if nr == 36 { args[1] } else { args[2] };
    if (old_pointer != 0 || nr == 36) && memory.check(old_pointer, 32, Access::Write).is_err() {
        return Ok(errno(14));
    }
    let next = if nr == 38 {
        let (interval, value) = if args[1] == 0 {
            (Duration::ZERO, Duration::ZERO)
        } else {
            if memory.check(args[1], 32, Access::Read).is_err() {
                return Ok(errno(14));
            }
            let a = match duration(&memory, args[1], true) {
                Ok(a) => a,
                Err(e) => return Ok(e),
            };
            let b = match duration(&memory, args[1] + 16, true) {
                Ok(a) => a,
                Err(e) => return Ok(e),
            };
            (a, b)
        };
        let deadline = if value.is_zero() {
            None
        } else {
            let Some(deadline) = Instant::now().checked_add(value) else {
                return Ok(errno(22));
            };
            Some(deadline)
        };
        Some(RealTimer { deadline, interval })
    } else {
        None
    };
    if old_pointer != 0 || nr == 36 {
        let mut old = [0; 32];
        timeval(&mut old[..16], timer.interval);
        timeval(&mut old[16..], timer.remaining());
        memory.write(old_pointer, &old)?;
    }
    if let Some(next) = next {
        *timer = next;
    }
    Ok(0)
}
pub(super) fn clock(process: &Process, nr: u64, args: [u64; 6]) -> Result<u64> {
    if matches!(nr, 228 | 229) && !matches!(args[0], 0 | 1 | 5 | 6) {
        return Err(Error::UnsupportedSyscall(nr));
    }
    let coarse = matches!(nr, 228 | 229) && matches!(args[0], 5 | 6);
    let (seconds, mut nanos) =
        blink_host::clock(nr == 96 || nr == 201 || matches!(args[0], 0 | 5), nr == 229);
    let seconds = if coarse && nr == 229 {
        nanos = 1_000_000;
        0
    } else {
        if coarse {
            nanos = nanos / 1_000_000 * 1_000_000;
        }
        seconds
    };
    let mut memory = process.memory.borrow_mut();
    if nr == 201 {
        if args[0] != 0 && memory.store(args[0], seconds as u64, 8).is_err() {
            return Ok(errno(14));
        }
        return Ok(seconds as u64);
    }
    let pointer = if nr == 96 { args[0] } else { args[1] };
    if nr == 229 && pointer == 0 {
        return Ok(0);
    }
    if (nr != 96 || pointer != 0) && memory.check(pointer, 16, Access::Write).is_err() {
        return Ok(errno(14));
    }
    if nr == 96 && args[1] != 0 && memory.check(args[1], 8, Access::Write).is_err() {
        return Ok(errno(14));
    }
    if nr != 96 || pointer != 0 {
        memory.store(pointer, seconds as u64, 8)?;
        memory.store(
            pointer + 8,
            if nr == 96 {
                (nanos / 1000) as u64
            } else {
                nanos as u64
            },
            8,
        )?;
    }
    if nr == 96 && args[1] != 0 {
        memory.store(args[1], 0, 8)?;
    }
    Ok(0)
}
pub(super) fn sleep(process: &mut Process, nr: u64, args: [u64; 6]) -> Result<Option<u64>> {
    if nr == 230 && (!matches!(args[0], 0 | 1) || args[1] & !1 != 0) {
        return Err(Error::UnsupportedSyscall(nr));
    }
    if process.sleep_deadline.is_none() {
        let request = if nr == 35 { args[0] } else { args[2] };
        let mut length = match duration(&process.memory.borrow(), request, false) {
            Ok(d) => d,
            Err(e) => return Ok(Some(e)),
        };
        if nr == 230 && args[1] == 1 {
            let (seconds, nanos) = blink_host::clock(args[0] == 0, false);
            let now = Duration::new(seconds as u64, nanos);
            length = length.saturating_sub(now);
        }
        let Some(deadline) = Instant::now().checked_add(length) else {
            return Ok(Some(errno(22)));
        };
        process.sleep_deadline = Some(deadline);
    }
    if process.sleep_deadline.is_some_and(|d| d <= Instant::now()) {
        process.sleep_deadline = None;
        Ok(Some(0))
    } else {
        Ok(None)
    }
}
pub(super) fn interrupt_sleep(process: &mut Process, nr: u64, args: [u64; 6]) -> u64 {
    let deadline = process.sleep_deadline.take();
    let pointer = if nr == 35 {
        args[1]
    } else if nr == 230 && args[1] == 0 {
        args[3]
    } else {
        0
    };
    if let (Some(deadline), true) = (deadline, pointer != 0) {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&left.as_secs().to_le_bytes());
        bytes[8..].copy_from_slice(&(left.subsec_nanos() as u64).to_le_bytes());
        if process.memory.borrow_mut().write(pointer, &bytes).is_err() {
            return errno(14);
        }
    }
    errno(4)
}
