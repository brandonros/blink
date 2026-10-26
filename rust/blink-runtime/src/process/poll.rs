//! Cooperative guest epoll and eventfd. No host epoll/eventfd or worker threads.
use super::*;
use blink_linux::{Epoll, EventCounter, Watch, WeakFd};

pub(super) fn create(process: &Process, nr: u64, args: [u64; 6], inode: &mut u64) -> Result<u64> {
    let counter = matches!(nr, 284 | 290);
    let flags = match nr {
        213 | 284 => 0,
        290 => args[1],
        _ => args[0],
    };
    let allowed = if counter {
        1 | 0x800 | 0x80000
    } else {
        0x80000
    };
    if flags & !allowed != 0 || nr == 213 && args[0] as i32 <= 0 {
        return Ok(errno(22));
    }
    let Some(fd) = process.fd() else {
        return Ok(errno(24));
    };
    let object = if counter {
        Fd::Counter(Rc::new(RefCell::new(EventCounter {
            inode: *inode,
            value: args[0] as u32 as u64,
            semaphore: flags & 1 != 0,
            nonblock: flags & 0x800 != 0,
        })))
    } else {
        Fd::Epoll(Rc::new(RefCell::new(Epoll {
            inode: *inode,
            watches: BTreeMap::new(),
            cursor: None,
        })))
    };
    *inode += 1;
    process.fds.borrow_mut().insert(fd, object);
    if flags & 0x80000 != 0 {
        process.descriptor_flags.borrow_mut().insert(fd);
    }
    Ok(fd as u64)
}
pub(super) fn counter_events(counter: &EventCounter) -> u16 {
    u16::from(counter.value != 0) | if counter.value < u64::MAX - 1 { 4 } else { 0 }
}
pub(super) fn transfer(
    memory: &mut Memory,
    counter: &mut EventCounter,
    address: u64,
    count: usize,
    write: bool,
) -> Result<Option<u64>> {
    if count < 8 {
        return Ok(Some(errno(22)));
    }
    if write {
        let value = match memory.load(address, 8) {
            Ok(v) => v,
            Err(_) => return Ok(Some(errno(14))),
        };
        if value == u64::MAX {
            return Ok(Some(errno(22)));
        }
        if value > u64::MAX - 1 - counter.value {
            return Ok(if counter.nonblock {
                Some(errno(11))
            } else {
                None
            });
        }
        counter.value += value;
    } else {
        if counter.value == 0 {
            return Ok(if counter.nonblock {
                Some(errno(11))
            } else {
                None
            });
        }
        let value = if counter.semaphore { 1 } else { counter.value };
        if memory.store(address, value, 8).is_err() {
            return Ok(Some(errno(14)));
        }
        counter.value -= value;
    }
    Ok(Some(8))
}
pub(super) fn control(process: &Process, args: [u64; 6], arch: GuestArch) -> Result<u64> {
    let fds = process.fds.borrow();
    let epoll = match fds.get(&(args[0] as i32)) {
        Some(Fd::Epoll(e)) => e.clone(),
        None => return Ok(errno(9)),
        _ => return Ok(errno(22)),
    };
    let Some(target) = fds.get(&(args[2] as i32)) else {
        return Ok(errno(9));
    };
    if args[0] as i32 == args[2] as i32 {
        return Ok(errno(22));
    }
    if matches!(target, Fd::Epoll(_)) {
        return Err(Error::UnsupportedSyscall(233));
    }
    let Some((key, target)) = WeakFd::new(args[2] as i32, target) else {
        return Ok(errno(1));
    };
    let mut epoll = epoll.borrow_mut();
    epoll
        .watches
        .retain(|_, watch| watch.target.upgrade().is_some());
    if args[1] == 2 {
        return Ok(if epoll.watches.remove(&key).is_some() {
            0
        } else {
            errno(2)
        });
    }
    if !matches!(args[1], 1 | 3) {
        return Ok(errno(22));
    }
    if args[1] == 1 && epoll.watches.contains_key(&key) {
        return Ok(errno(17));
    }
    if args[1] == 3 && !epoll.watches.contains_key(&key) {
        return Ok(errno(2));
    }
    let size = if arch == GuestArch::X86_64 { 12 } else { 16 };
    let memory = process.memory.borrow();
    if memory.check(args[3], size, Access::Read).is_err() {
        return Ok(errno(14));
    }
    let events = memory.load(args[3], 4)? as u32;
    // Edge-triggering, exclusive wakeup and nested epoll need separate state
    // semantics; reject them explicitly rather than pretending level behavior.
    if events & !(0x40000000 | 0x2000 | 0x3ff) != 0 {
        return Err(Error::UnsupportedSyscall(233));
    }
    let data = memory.load(args[3] + if size == 12 { 4 } else { 8 }, 8)?;
    if epoll.watches.len() >= 4096 && args[1] == 1 {
        return Ok(errno(28));
    }
    epoll.watches.insert(
        key,
        Watch {
            target,
            events,
            data,
            enabled: true,
        },
    );
    Ok(0)
}
pub(super) fn readable(epoll: &Epoll) -> bool {
    epoll.watches.values().any(|watch| {
        watch.enabled
            && watch
                .target
                .upgrade()
                .is_some_and(|fd| super::poll_events(Some(&fd), watch.events as u16) != 0)
    })
}
pub(super) fn wait(
    process: &mut Process,
    nr: u64,
    args: [u64; 6],
    arch: GuestArch,
) -> Result<Option<u64>> {
    let epoll = match process.fds.borrow().get(&(args[0] as i32)) {
        Some(Fd::Epoll(e)) => e.clone(),
        None => return Ok(Some(errno(9))),
        _ => return Ok(Some(errno(22))),
    };
    if args[2] == 0 || args[2] > i32::MAX as u64 / 12 {
        return Ok(Some(errno(22)));
    }
    if nr == 281 && args[4] != 0 && process.suspend_mask.is_none() {
        if args[5] != 8 {
            return Ok(Some(errno(22)));
        }
        let mask = match process.memory.borrow().load(args[4], 8) {
            Ok(mask) => mask,
            Err(_) => return Ok(Some(errno(14))),
        };
        process.suspend_mask = Some(process.signal_mask);
        process.signal_mask = mask & !UNBLOCKABLE;
        // Give pending signals a delivery point before sleeping or returning events.
        if process.signals.bits & !process.signal_mask != 0 {
            return Ok(None);
        }
    }
    let mut epoll = epoll.borrow_mut();
    epoll
        .watches
        .retain(|_, watch| watch.target.upgrade().is_some());
    let mut keys: Vec<_> = epoll.watches.keys().copied().collect();
    if let Some(cursor) = epoll.cursor {
        let split = keys.partition_point(|key| *key <= cursor);
        keys.rotate_left(split);
    }
    let size = if arch == GuestArch::X86_64 { 12 } else { 16 };
    let mut bytes = Vec::new();
    let mut delivered = Vec::new();
    for key in keys {
        let watch = &epoll.watches[&key];
        if !watch.enabled {
            continue;
        }
        let Some(fd) = watch.target.upgrade() else {
            continue;
        };
        let events = super::poll_events(Some(&fd), watch.events as u16) as u32;
        if events == 0 {
            continue;
        }
        let offset = bytes.len();
        bytes.resize(offset + size, 0);
        bytes[offset..offset + 4].copy_from_slice(&events.to_le_bytes());
        let data = offset + if size == 12 { 4 } else { 8 };
        bytes[data..data + 8].copy_from_slice(&watch.data.to_le_bytes());
        delivered.push(key);
        if delivered.len() as u64 == args[2] {
            break;
        }
    }
    let timeout = args[3] as i32;
    if !delivered.is_empty() {
        if process.memory.borrow_mut().write(args[1], &bytes).is_err() {
            return Ok(Some(errno(14)));
        }
        for key in &delivered {
            let watch = epoll.watches.get_mut(key).unwrap();
            if watch.events & 0x40000000 != 0 {
                watch.enabled = false;
            }
        }
        epoll.cursor = delivered.last().copied();
        process.poll_deadline = None;
        return Ok(Some(delivered.len() as u64));
    }
    if timeout == 0 || process.poll_deadline.is_some_and(|d| Instant::now() >= d) {
        process.poll_deadline = None;
        return Ok(Some(0));
    }
    if timeout > 0 && process.poll_deadline.is_none() {
        process.poll_deadline =
            Instant::now().checked_add(std::time::Duration::from_millis(timeout as u64));
    }
    Ok(None)
}
