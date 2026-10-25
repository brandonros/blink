//! Nonblocking host TCP operations. Guest ABI values and scheduling stay in the
//! Linux/runtime layers. Every handle is owned and never duplicates a host task.
use rustix::{
    event::{poll, PollFd, PollFlags, Timespec},
    fd::OwnedFd,
    io::Errno,
    net::{self, sockopt, AddressFamily, SocketFlags, SocketType},
};
use std::{io, net::SocketAddr};

pub struct Tcp {
    fd: OwnedFd,
}
#[derive(Default)]
pub struct Ready {
    pub read: bool,
    pub write: bool,
    pub error: bool,
    pub hangup: bool,
}
impl Tcp {
    pub fn new(ipv6: bool) -> io::Result<Self> {
        let fd = net::socket_with(
            if ipv6 {
                AddressFamily::INET6
            } else {
                AddressFamily::INET
            },
            SocketType::STREAM,
            SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
            Some(net::ipproto::TCP),
        )?;
        #[cfg(target_vendor = "apple")]
        sockopt::set_socket_nosigpipe(&fd, true)?;
        Ok(Self { fd })
    }
    /// True means connected; false means a nonblocking connection is pending.
    pub fn connect(&self, address: &SocketAddr) -> io::Result<bool> {
        match net::connect(&self.fd, address) {
            Ok(()) => Ok(true),
            Err(Errno::INPROGRESS | Errno::ALREADY | Errno::WOULDBLOCK) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
    pub fn ready(&self) -> io::Result<Ready> {
        let mut fds = [PollFd::new(&self.fd, PollFlags::IN | PollFlags::OUT)];
        poll(
            &mut fds,
            Some(&Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            }),
        )?;
        let flags = fds[0].revents();
        Ok(Ready {
            read: flags.contains(PollFlags::IN),
            write: flags.contains(PollFlags::OUT),
            error: flags.contains(PollFlags::ERR),
            hangup: flags.contains(PollFlags::HUP),
        })
    }
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        Ok(sockopt::socket_error(&self.fd)?.err().map(Into::into))
    }
    pub fn receive(&self, bytes: &mut [u8], peek: bool) -> io::Result<usize> {
        let flags = if peek {
            net::RecvFlags::PEEK
        } else {
            net::RecvFlags::empty()
        };
        net::recv(&self.fd, bytes, flags)
            .map(|(_, n)| n)
            .map_err(Into::into)
    }
    pub fn send(&self, bytes: &[u8]) -> io::Result<usize> {
        #[cfg(target_vendor = "apple")]
        let flags = net::SendFlags::empty();
        #[cfg(not(target_vendor = "apple"))]
        let flags = net::SendFlags::NOSIGNAL;
        net::send(&self.fd, bytes, flags).map_err(Into::into)
    }
    pub fn shutdown(&self, how: u64) -> io::Result<()> {
        net::shutdown(
            &self.fd,
            match how {
                0 => net::Shutdown::Read,
                1 => net::Shutdown::Write,
                _ => net::Shutdown::Both,
            },
        )
        .map_err(Into::into)
    }
    pub fn address(&self, peer: bool) -> io::Result<SocketAddr> {
        let address = if peer {
            net::getpeername(&self.fd)?.ok_or(io::ErrorKind::NotConnected)?
        } else {
            net::getsockname(&self.fd)?
        };
        address
            .try_into()
            .map_err(|_| io::ErrorKind::InvalidData.into())
    }
    pub fn nodelay(&self, value: bool) -> io::Result<()> {
        sockopt::set_tcp_nodelay(&self.fd, value).map_err(Into::into)
    }
    pub fn keepalive(&self, value: bool) -> io::Result<()> {
        sockopt::set_socket_keepalive(&self.fd, value).map_err(Into::into)
    }
}
