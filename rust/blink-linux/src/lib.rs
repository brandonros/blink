//! Shared Linux kernel objects, independent of guest register and structure layouts.
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
#[derive(Default)]
pub struct Pipe {
    pub inode: u64,
    pub bytes: VecDeque<u8>,
    pub readers: usize,
    pub writers: usize,
}
/// A connected guest stream endpoint. Duplicates share status flags and shutdown
/// state; the last descriptor closes each live half, including after fork/exec.
pub struct Socket {
    pub inode: u64,
    pub receive: Rc<RefCell<Pipe>>,
    pub send: Rc<RefCell<Pipe>>,
    pub nonblock: std::cell::Cell<bool>,
    pub read_closed: std::cell::Cell<bool>,
    pub write_closed: std::cell::Cell<bool>,
}
impl Socket {
    pub fn pair(inode: u64, nonblock: bool) -> [Rc<Self>; 2] {
        let queues: [_; 2] = std::array::from_fn(|_| {
            Rc::new(RefCell::new(Pipe {
                readers: 1,
                writers: 1,
                ..Default::default()
            }))
        });
        std::array::from_fn(|n| {
            Rc::new(Self {
                inode: inode + n as u64,
                receive: queues[n].clone(),
                send: queues[1 - n].clone(),
                nonblock: nonblock.into(),
                read_closed: false.into(),
                write_closed: false.into(),
            })
        })
    }
    pub fn shutdown(&self, how: u64) {
        if how != 1 && !self.read_closed.replace(true) {
            self.receive.borrow_mut().readers -= 1;
        }
        if how != 0 && !self.write_closed.replace(true) {
            self.send.borrow_mut().writers -= 1;
        }
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        self.shutdown(2);
    }
}
pub enum VirtualContent {
    Bytes(std::io::Cursor<Vec<u8>>),
    Directory {
        entries: Vec<blink_host::DirectoryEntry>,
        position: usize,
    },
    Null,
    Zero,
    Random,
}
pub struct VirtualOpen {
    pub path: std::path::PathBuf,
    pub stat: blink_host::FileStat,
    pub content: VirtualContent,
    pub read: bool,
    pub write: bool,
}
pub struct InternetSocket {
    pub host: blink_host::net::Tcp,
    pub inode: u64,
    pub nonblock: bool,
    pub connecting: bool,
    pub connected: bool,
    pub ipv6: bool,
}
pub enum Fd {
    Internet(Rc<RefCell<InternetSocket>>),
    Socket(Rc<Socket>),
    Virtual(Rc<RefCell<VirtualOpen>>),
    Directory(Rc<RefCell<blink_host::Directory>>),
    UnixSocket {
        inode: u64,
        nonblock: bool,
    },
    File {
        file: Rc<RefCell<std::fs::File>>,
        read: bool,
        write: bool,
        cloexec: bool,
        append: bool,
    },
    Stdin,
    Stdout,
    Stderr,
    Reader(Rc<RefCell<Pipe>>),
    Writer(Rc<RefCell<Pipe>>),
}
impl Clone for Fd {
    fn clone(&self) -> Self {
        match self {
            Self::Socket(s) => Self::Socket(s.clone()),
            Self::Internet(s) => Self::Internet(s.clone()),
            Self::Virtual(v) => Self::Virtual(v.clone()),
            Self::Directory(d) => Self::Directory(d.clone()),
            Self::UnixSocket { inode, nonblock } => Self::UnixSocket {
                inode: *inode,
                nonblock: *nonblock,
            },
            Self::File {
                file,
                read,
                write,
                cloexec,
                append,
            } => Self::File {
                file: file.clone(),
                read: *read,
                write: *write,
                cloexec: *cloexec,
                append: *append,
            },
            Self::Stdin => Self::Stdin,
            Self::Stdout => Self::Stdout,
            Self::Stderr => Self::Stderr,
            Self::Reader(p) => {
                p.borrow_mut().readers += 1;
                Self::Reader(p.clone())
            }
            Self::Writer(p) => {
                p.borrow_mut().writers += 1;
                Self::Writer(p.clone())
            }
        }
    }
}
impl Drop for Fd {
    fn drop(&mut self) {
        match self {
            Self::Reader(p) => p.borrow_mut().readers -= 1,
            Self::Writer(p) => p.borrow_mut().writers -= 1,
            _ => {}
        }
    }
}

impl Fd {
    pub fn duplicate(&self) -> Self {
        let mut fd = self.clone();
        if let Self::File { cloexec, .. } = &mut fd {
            *cloexec = false;
        }
        fd
    }
    pub fn close_on_exec(&self) -> bool {
        matches!(self, Self::File { cloexec: true, .. })
    }
}
/// Translate host errors by kind, never by host errno numeric value.
pub fn errno(error: &std::io::Error) -> u64 {
    use std::io::ErrorKind::*;
    let n: i64 = match error.kind() {
        NotFound => 2,
        PermissionDenied => 13,
        AlreadyExists => 17,
        DirectoryNotEmpty => 39,
        CrossesDevices => 18,
        TooManyLinks => 31,
        InvalidInput | InvalidData => 22,
        NotADirectory => 20,
        IsADirectory => 21,
        StorageFull => 28,
        ReadOnlyFilesystem => 30,
        Interrupted => 4,
        WouldBlock => 11,
        BrokenPipe => 32,
        NotConnected => 107,
        ConnectionRefused => 111,
        ConnectionReset => 104,
        ConnectionAborted => 103,
        TimedOut => 110,
        AddrInUse => 98,
        AddrNotAvailable => 99,
        NetworkUnreachable => 101,
        HostUnreachable => 113,
        Unsupported => 38,
        _ => 5,
    };
    (-n) as u64
}

#[derive(Clone, Copy, Default)]
pub struct SignalAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}
pub const UNBLOCKABLE: u64 = (1 << 8) | (1 << 18);

impl Fd {
    pub fn stat(&self, rootless: bool) -> std::io::Result<blink_host::FileStat> {
        use blink_host::FileStat;
        match self {
            Self::Virtual(v) => Ok(v.borrow().stat.clone()),
            Self::File { file, .. } => {
                blink_host::metadata::Target::File(&file.borrow()).stat(rootless)
            }
            Self::Directory(d) => {
                blink_host::metadata::Target::File(&d.borrow().file).stat(rootless)
            }
            _ => Ok(FileStat {
                dev: 0xffff_ff00,
                ino: match self {
                    Self::Socket(s) => s.inode,
                    Self::Internet(s) => s.borrow().inode,
                    Self::UnixSocket { inode, .. } => *inode,
                    Self::Stdin => 1,
                    Self::Stdout => 2,
                    Self::Stderr => 3,
                    Self::Reader(p) | Self::Writer(p) => p.borrow().inode,
                    _ => unreachable!(),
                },
                nlink: 1,
                mode: if matches!(
                    self,
                    Self::UnixSocket { .. } | Self::Socket(_) | Self::Internet(_)
                ) {
                    0o140600
                } else {
                    0o010600
                },
                blksize: 4096,
                ..Default::default()
            }),
        }
    }
}
pub fn stat_bytes(s: &blink_host::FileStat, arch: blink_core::GuestArch) -> Vec<u8> {
    let mut bytes = vec![
        0;
        if arch == blink_core::GuestArch::X86_64 {
            144
        } else {
            128
        }
    ];
    let mut put = |offset: usize, width: usize, value: u64| {
        bytes[offset..offset + width].copy_from_slice(&value.to_le_bytes()[..width])
    };
    put(0, 8, s.dev);
    put(8, 8, s.ino);
    if arch == blink_core::GuestArch::X86_64 {
        put(16, 8, s.nlink);
        put(24, 4, s.mode as u64);
        put(28, 4, s.uid as u64);
        put(32, 4, s.gid as u64);
        put(40, 8, s.rdev);
        put(56, 8, s.blksize);
    } else {
        put(16, 4, s.mode as u64);
        put(20, 4, s.nlink);
        put(24, 4, s.uid as u64);
        put(28, 4, s.gid as u64);
        put(32, 8, s.rdev);
        put(56, 4, s.blksize);
    }
    put(48, 8, s.size);
    put(64, 8, s.blocks);
    put(72, 8, s.atime as u64);
    put(80, 8, s.atime_nsec as u64);
    put(88, 8, s.mtime as u64);
    put(96, 8, s.mtime_nsec as u64);
    put(104, 8, s.ctime as u64);
    put(112, 8, s.ctime_nsec as u64);
    bytes
}

#[derive(Clone)]
pub struct Credentials {
    pub uid: u32,
    pub euid: u32,
    pub suid: u32,
    pub gid: u32,
    pub egid: u32,
    pub sgid: u32,
    pub groups: Vec<u32>,
}
impl Credentials {
    pub fn new(uid: u32, gid: u32) -> Self {
        Self {
            uid,
            euid: uid,
            suid: uid,
            gid,
            egid: gid,
            sgid: gid,
            groups: Vec::new(),
        }
    }
}

/// Both supported 64-bit Linux guest ABIs use this statfs layout.
pub fn statfs_bytes(stat: &blink_host::FilesystemStat) -> [u8; 120] {
    let mut bytes = [0; 120];
    let fields = [
        stat.kind,
        stat.block_size,
        stat.blocks,
        stat.free_blocks,
        stat.available_blocks,
        stat.files,
        stat.free_files,
        stat.id,
        stat.name_max,
        stat.fragment_size,
        0x26 | u64::from(stat.readonly),
    ];
    for (n, value) in fields.iter().enumerate() {
        bytes[n * 8..n * 8 + 8].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}
