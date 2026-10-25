//! Guest-only AF_UNIX stream pairs; no host networking or host process state.
use super::*;
use blink_linux::Socket;

pub(super) fn pair(process: &Process, args: [u64; 6], next_inode: &mut u64) -> Result<u64> {
    if args[0] != 1 || args[1] & !(0x80000 | 0x800) != 1 || args[2] != 0 {
        return Err(Error::UnsupportedSyscall(53));
    }
    if process
        .memory
        .borrow()
        .check(args[3], 8, Access::Write)
        .is_err()
    {
        return Ok(errno(14));
    }
    let vacant: Vec<_> = (0..1024)
        .filter(|fd| !process.fds.borrow().contains_key(fd))
        .take(2)
        .collect();
    if vacant.len() != 2 {
        return Ok(errno(24));
    }
    for (index, (fd, endpoint)) in vacant
        .into_iter()
        .zip(Socket::pair(*next_inode, args[1] & 0x800 != 0))
        .enumerate()
    {
        process.fds.borrow_mut().insert(fd, Fd::Socket(endpoint));
        if args[1] & 0x80000 != 0 {
            process.descriptor_flags.borrow_mut().insert(fd);
        }
        process
            .memory
            .borrow_mut()
            .store(args[3] + index as u64 * 4, fd as u64, 4)?;
    }
    *next_inode += 2;
    Ok(0)
}

/// None requests a cooperative wait. Memory is checked before consuming data.
pub(super) fn transfer(
    memory: &mut Memory,
    socket: &Socket,
    address: u64,
    count: u64,
    write: bool,
    flags: u64,
) -> Result<Option<u64>> {
    let allowed = if write { 0x40 | 0x4000 } else { 0x40 | 2 };
    if flags & !allowed != 0 {
        return Err(Error::UnsupportedSyscall(if write { 44 } else { 45 }));
    }
    let count = count.min(0x7ffff000) as usize;
    let mut queue = if write {
        socket.send.borrow_mut()
    } else {
        socket.receive.borrow_mut()
    };
    if write && (socket.write_closed.get() || queue.readers == 0) {
        return Ok(Some(errno(32)));
    }
    if count == 0 {
        return Ok(Some(0));
    }
    let count = if write {
        count.min(65536 - queue.bytes.len())
    } else {
        if queue.bytes.is_empty() && (socket.read_closed.get() || queue.writers == 0) {
            return Ok(Some(0));
        }
        count.min(queue.bytes.len())
    };
    if count == 0 {
        return Ok(if socket.nonblock.get() || flags & 0x40 != 0 {
            Some(errno(11))
        } else {
            None
        });
    }
    if memory
        .check(
            address,
            count,
            if write { Access::Read } else { Access::Write },
        )
        .is_err()
    {
        return Ok(Some(errno(14)));
    }
    if write {
        let mut bytes = vec![0; count];
        memory.read(address, &mut bytes, Access::Read)?;
        queue.bytes.extend(bytes);
    } else {
        let bytes: Vec<_> = queue.bytes.iter().take(count).copied().collect();
        memory.write(address, &bytes)?;
        if flags & 2 == 0 {
            queue.bytes.drain(..count);
        }
    }
    Ok(Some(count as u64))
}

pub(super) fn vectored(
    memory: &mut Memory,
    socket: &Socket,
    address: u64,
    count: u64,
    write: bool,
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
    let mut vectors = Vec::new();
    let mut total = 0;
    for n in 0..count {
        let base = memory.load(address + n * 16, 8)?;
        let length = memory.load(address + n * 16 + 8, 8)?;
        if length > 0x7ffff000 || total + length > 0x7ffff000 {
            return Ok(Some(errno(22)));
        }
        total += length;
        vectors.push((base, length));
    }
    let mut done = 0;
    for (base, length) in vectors {
        match transfer(memory, socket, base, length, write, 0)? {
            Some(n) if (n as i64) >= 0 => {
                done += n;
                if n != length {
                    break;
                }
            }
            result => return Ok(if done > 0 { Some(done) } else { result }),
        }
    }
    Ok(Some(done))
}

pub(super) fn name(process: &Process, args: [u64; 6]) -> Result<u64> {
    let mut memory = process.memory.borrow_mut();
    if memory.check(args[2], 4, Access::Write).is_err() {
        return Ok(errno(14));
    }
    let length = match memory.load(args[2], 4) {
        Ok(n) => n,
        Err(_) => return Ok(errno(14)),
    };
    let bytes = &1u16.to_le_bytes()[..length.min(2) as usize];
    if memory.write(args[1], bytes).is_err() {
        return Ok(errno(14));
    }
    memory.store(args[2], 2, 4)?;
    Ok(0)
}

pub(super) fn events(socket: &Socket) -> u16 {
    let receive = socket.receive.borrow();
    let send = socket.send.borrow();
    let eof = socket.read_closed.get() || receive.writers == 0;
    let broken = socket.write_closed.get() || send.readers == 0;
    (if eof || !receive.bytes.is_empty() {
        1 | 64
    } else {
        0
    }) | (if eof { 0x2000 } else { 0 })
        | (if broken || send.bytes.len() < 65536 {
            4 | 256
        } else {
            0
        })
        | (if send.readers == 0 { 8 } else { 0 })
        | (if eof && (socket.write_closed.get() || send.readers == 0) {
            16
        } else {
            0
        })
}
