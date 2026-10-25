//! Explicit opt-in TCP forwarding. Host handles stay nonblocking even when the
//! guest descriptor is blocking, so waits participate in cancellation/scheduling.
use super::*;
use blink_linux::InternetSocket;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub(super) fn create(
    config: &RuntimeConfig,
    process: &Process,
    args: [u64; 6],
    inode: &mut u64,
) -> Result<u64> {
    if !config.network {
        return Ok(errno(13));
    }
    if !matches!(args[0], 2 | 10) || args[1] & !(0x800 | 0x80000) != 1 || !matches!(args[2], 0 | 6)
    {
        return Err(Error::UnsupportedSyscall(41));
    }
    let Some(fd) = process.fd() else {
        return Ok(errno(24));
    };
    match blink_host::net::Tcp::new(args[0] == 10) {
        Err(error) => Ok(blink_linux::errno(&error)),
        Ok(host) => {
            process.fds.borrow_mut().insert(
                fd,
                Fd::Internet(Rc::new(RefCell::new(InternetSocket {
                    host,
                    inode: *inode,
                    nonblock: args[1] & 0x800 != 0,
                    connecting: false,
                    connected: false,
                    ipv6: args[0] == 10,
                }))),
            );
            *inode += 1;
            if args[1] & 0x80000 != 0 {
                process.descriptor_flags.borrow_mut().insert(fd);
            }
            Ok(fd as u64)
        }
    }
}
fn address(memory: &Memory, pointer: u64, length: u64) -> std::result::Result<SocketAddr, u64> {
    if length < 2 {
        return Err(errno(22));
    }
    let family = memory.load(pointer, 2).map_err(|_| errno(14))?;
    let needed = match family {
        2 => 16,
        10 => 28,
        _ => return Err(errno(97)),
    };
    if length < needed {
        return Err(errno(22));
    }
    let mut bytes = vec![0; needed as usize];
    memory
        .read(pointer, &mut bytes, Access::Read)
        .map_err(|_| errno(14))?;
    let port = u16::from_be_bytes(bytes[2..4].try_into().unwrap());
    Ok(if family == 2 {
        SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7])),
            port,
        )
    } else {
        let addr = Ipv6Addr::from(<[u8; 16]>::try_from(&bytes[8..24]).unwrap());
        SocketAddr::V6(std::net::SocketAddrV6::new(
            addr,
            port,
            u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
        ))
    })
}
pub(super) fn connect(
    memory: &Memory,
    socket: &mut InternetSocket,
    args: [u64; 6],
) -> Result<Option<u64>> {
    let address = match address(memory, args[1], args[2]) {
        Ok(a) => a,
        Err(e) => return Ok(Some(e)),
    };
    if address.is_ipv6() != socket.ipv6 {
        return Ok(Some(errno(97)));
    }
    if socket.connected {
        return Ok(Some(errno(106)));
    }
    if !socket.connecting {
        match socket.host.connect(&address) {
            Ok(true) => {
                socket.connected = true;
                return Ok(Some(0));
            }
            Ok(false) => {
                socket.connecting = true;
                if socket.nonblock {
                    return Ok(Some(errno(115)));
                }
            }
            Err(error) => return Ok(Some(blink_linux::errno(&error))),
        }
    }
    let ready = socket.host.ready()?;
    if !ready.write && !ready.error && !ready.hangup {
        return Ok(if socket.nonblock {
            Some(errno(114))
        } else {
            None
        });
    }
    socket.connecting = false;
    if let Some(error) = socket.host.take_error()? {
        return Ok(Some(blink_linux::errno(&error)));
    }
    socket.connected = true;
    Ok(Some(0))
}
pub(super) fn transfer(
    memory: &mut Memory,
    socket: &InternetSocket,
    address: u64,
    count: u64,
    write: bool,
    flags: u64,
) -> Result<Option<u64>> {
    let allowed = if write { 0x40 | 0x4000 } else { 0x40 | 2 };
    if flags & !allowed != 0 {
        return Err(Error::UnsupportedSyscall(if write { 44 } else { 45 }));
    }
    let count = count.min(65536) as usize;
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
    let mut bytes = vec![0; count];
    let result = if write {
        memory.read(address, &mut bytes, Access::Read)?;
        socket.host.send(&bytes)
    } else {
        socket.host.receive(&mut bytes, flags & 2 != 0)
    };
    match result {
        Ok(n) => {
            if !write {
                memory.write(address, &bytes[..n])?;
            }
            Ok(Some(n as u64))
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::WouldBlock
                && !socket.nonblock
                && flags & 0x40 == 0 =>
        {
            Ok(None)
        }
        Err(error) => Ok(Some(blink_linux::errno(&error))),
    }
}
pub(super) fn vectored(
    memory: &mut Memory,
    socket: &InternetSocket,
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
pub(super) fn name(
    process: &Process,
    socket: &InternetSocket,
    args: [u64; 6],
    peer: bool,
) -> Result<u64> {
    let address = match socket.host.address(peer) {
        Ok(a) => a,
        Err(e) => return Ok(blink_linux::errno(&e)),
    };
    let mut bytes = vec![0; if address.is_ipv4() { 16 } else { 28 }];
    bytes[..2].copy_from_slice(&(if address.is_ipv4() { 2u16 } else { 10 }).to_le_bytes());
    bytes[2..4].copy_from_slice(&address.port().to_be_bytes());
    match address {
        SocketAddr::V4(a) => bytes[4..8].copy_from_slice(&a.ip().octets()),
        SocketAddr::V6(a) => {
            bytes[4..8].copy_from_slice(&a.flowinfo().to_be_bytes());
            bytes[8..24].copy_from_slice(&a.ip().octets());
            bytes[24..28].copy_from_slice(&a.scope_id().to_le_bytes());
        }
    }
    let mut memory = process.memory.borrow_mut();
    let length = match memory.load(args[2], 4) {
        Ok(n) => n as usize,
        Err(_) => return Ok(errno(14)),
    };
    if memory.check(args[2], 4, Access::Write).is_err()
        || memory
            .write(args[1], &bytes[..length.min(bytes.len())])
            .is_err()
    {
        return Ok(errno(14));
    }
    memory.store(args[2], bytes.len() as u64, 4)?;
    Ok(0)
}
pub(super) fn option(
    process: &Process,
    socket: &mut InternetSocket,
    args: [u64; 6],
    set: bool,
) -> Result<u64> {
    let mut memory = process.memory.borrow_mut();
    if set {
        if args[4] < 4 {
            return Ok(errno(22));
        }
        let value = match memory.load(args[3], 4) {
            Ok(n) => n != 0,
            Err(_) => return Ok(errno(14)),
        };
        let result = match (args[1], args[2]) {
            (1, 9) => socket.host.keepalive(value),
            (6, 1) => socket.host.nodelay(value),
            _ => return Err(Error::UnsupportedSyscall(54)),
        };
        return Ok(result.map_or_else(|e| blink_linux::errno(&e), |_| 0));
    }
    let length = match memory.load(args[4], 4) {
        Ok(n) => n as usize,
        Err(_) => return Ok(errno(14)),
    };
    if memory.check(args[4], 4, Access::Write).is_err()
        || memory.check(args[3], length.min(4), Access::Write).is_err()
    {
        return Ok(errno(14));
    }
    let value = match (args[1], args[2]) {
        (1, 3) => 1u32,
        (1, 4) => match socket.host.take_error()? {
            None => {
                if socket.connecting && socket.host.address(true).is_ok() {
                    socket.connected = true;
                    socket.connecting = false;
                }
                0
            }
            Some(error) => {
                socket.connecting = false;
                (-(blink_linux::errno(&error) as i64)) as u32
            }
        },
        (1, 30) => 0,
        (1, 38) => 6,
        (1, 39) => {
            if socket.ipv6 {
                10
            } else {
                2
            }
        }
        _ => return Err(Error::UnsupportedSyscall(55)),
    };
    memory.write(args[3], &value.to_le_bytes()[..length.min(4)])?;
    memory.store(args[4], length.min(4) as u64, 4)?;
    Ok(0)
}
pub(super) fn events(socket: &InternetSocket) -> u16 {
    let Ok(ready) = socket.host.ready() else {
        return 32;
    };
    let eof = ready.read && matches!(socket.host.receive(&mut [0; 1], true), Ok(0));
    (if ready.read { 1 | 64 } else { 0 })
        | (if ready.write { 4 | 256 } else { 0 })
        | (if ready.error { 8 } else { 0 })
        | (if ready.hangup { 16 } else { 0 })
        | (if eof { 0x2000 } else { 0 })
}
