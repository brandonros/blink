//! Shared Linux kernel objects, independent of guest register and structure layouts.
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
#[derive(Default)]
pub struct Pipe {
    pub bytes: VecDeque<u8>,
    pub readers: usize,
    pub writers: usize,
}
pub enum Fd {
    File {
        file: Rc<RefCell<std::fs::File>>,
        read: bool,
        write: bool,
        cloexec: bool,
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
            Self::File {
                file,
                read,
                write,
                cloexec,
            } => Self::File {
                file: file.clone(),
                read: *read,
                write: *write,
                cloexec: *cloexec,
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
        InvalidInput | InvalidData => 22,
        NotADirectory => 20,
        IsADirectory => 21,
        StorageFull => 28,
        ReadOnlyFilesystem => 30,
        Interrupted => 4,
        WouldBlock => 11,
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
