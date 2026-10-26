//! Guest event counters and epoll watches. Weak references do not keep watched
//! descriptions alive after the last guest descriptor closes.
use super::*;
use std::{collections::BTreeMap, rc::Weak};
pub struct EventCounter {
    pub inode: u64,
    pub value: u64,
    pub semaphore: bool,
    pub nonblock: bool,
}
pub type WatchKey = (i32, usize, u8);
pub struct Epoll {
    pub inode: u64,
    pub watches: BTreeMap<WatchKey, Watch>,
    pub cursor: Option<WatchKey>,
}
pub struct Watch {
    pub target: WeakFd,
    pub events: u32,
    pub data: u64,
    pub enabled: bool,
}
pub enum WeakFd {
    Reader(Weak<RefCell<Pipe>>),
    Writer(Weak<RefCell<Pipe>>),
    Socket(Weak<Socket>),
    Internet(Weak<RefCell<InternetSocket>>),
    Counter(Weak<RefCell<EventCounter>>),
}
impl WeakFd {
    pub fn new(number: i32, fd: &Fd) -> Option<(WatchKey, Self)> {
        Some(match fd {
            Fd::Reader(p) => (
                (number, Rc::as_ptr(p) as usize, 0),
                Self::Reader(Rc::downgrade(p)),
            ),
            Fd::Writer(p) => (
                (number, Rc::as_ptr(p) as usize, 1),
                Self::Writer(Rc::downgrade(p)),
            ),
            Fd::Socket(s) => (
                (number, Rc::as_ptr(s) as usize, 2),
                Self::Socket(Rc::downgrade(s)),
            ),
            Fd::Internet(s) => (
                (number, Rc::as_ptr(s) as usize, 3),
                Self::Internet(Rc::downgrade(s)),
            ),
            Fd::Counter(c) => (
                (number, Rc::as_ptr(c) as usize, 4),
                Self::Counter(Rc::downgrade(c)),
            ),
            _ => return None,
        })
    }
    pub fn upgrade(&self) -> Option<Fd> {
        Some(match self {
            Self::Reader(w) => {
                let p = w.upgrade()?;
                if p.borrow().readers == 0 {
                    return None;
                }
                p.borrow_mut().readers += 1;
                Fd::Reader(p)
            }
            Self::Writer(w) => {
                let p = w.upgrade()?;
                if p.borrow().writers == 0 {
                    return None;
                }
                p.borrow_mut().writers += 1;
                Fd::Writer(p)
            }
            Self::Socket(w) => Fd::Socket(w.upgrade()?),
            Self::Internet(w) => Fd::Internet(w.upgrade()?),
            Self::Counter(w) => Fd::Counter(w.upgrade()?),
        })
    }
}
