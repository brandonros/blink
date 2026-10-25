//! Shared contracts. Guest addresses are never host pointers.
use std::{collections::BTreeMap, fmt};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestArch {
    X86_64,
    Aarch64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidElf(String),
    MemoryFault { address: u64, access: Access },
    UnsupportedInstruction { pc: u64, encoding: u32 },
    UnsupportedSyscall(u64),
    ArithmeticFault,
    BusFault { address: u64 },
    ResourceLimit,
    InvalidConfig(String),
    Closed,
    Io(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Execute,
}
#[derive(Clone, Copy, Debug)]
pub struct Permissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}
impl Permissions {
    pub const RW: Self = Self {
        read: true,
        write: true,
        execute: false,
    };
    pub const RX: Self = Self {
        read: true,
        write: false,
        execute: true,
    };
    fn allows(self, access: Access) -> bool {
        match access {
            Access::Read => self.read,
            Access::Write => self.write,
            Access::Execute => self.execute,
        }
    }
}
const PAGE: u64 = 4096;
#[derive(Clone)]
struct Page {
    bytes: Box<[u8; 4096]>,
    permissions: Permissions,
    bus_fault: bool,
    inherit_on_fork: bool,
}
#[derive(Clone)]
pub struct Memory {
    pages: BTreeMap<u64, Page>,
    max_pages: usize,
}
impl Memory {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            pages: BTreeMap::new(),
            max_pages: max_bytes / 4096,
        }
    }
    pub fn mapped_bytes(&self) -> usize {
        self.pages.len() * 4096
    }
    pub fn map(&mut self, start: u64, size: u64, permissions: Permissions) -> Result<()> {
        if size == 0 {
            return Ok(());
        }
        let end = start.checked_add(size - 1).ok_or(Error::ResourceLimit)?;
        let first = start / PAGE;
        let last = end / PAGE;
        let count = last
            .checked_sub(first)
            .and_then(|v| v.checked_add(1))
            .ok_or(Error::ResourceLimit)?;
        if count > self.max_pages as u64 || self.pages.len() as u64 + count > self.max_pages as u64
        {
            return Err(Error::ResourceLimit);
        }
        if (first..=last).any(|p| self.pages.contains_key(&p)) {
            return Err(Error::InvalidElf("overlapping mapped pages".into()));
        }
        for p in first..=last {
            self.pages.insert(
                p,
                Page {
                    bytes: Box::new([0; 4096]),
                    permissions,
                    bus_fault: false,
                    inherit_on_fork: true,
                },
            );
        }
        Ok(())
    }
    /// Deep-copy inherited mappings for a Linux fork. Ordinary Clone remains
    /// an exact copy; shared-address-space threads/vfork do not use this method.
    pub fn fork_copy(&self) -> Self {
        Self {
            pages: self
                .pages
                .iter()
                .filter(|(_, page)| page.inherit_on_fork)
                .map(|(address, page)| (*address, page.clone()))
                .collect(),
            max_pages: self.max_pages,
        }
    }
    pub fn fork_bytes(&self) -> usize {
        self.pages
            .values()
            .filter(|page| page.inherit_on_fork)
            .count()
            * 4096
    }
    pub fn set_fork_inheritance(&mut self, address: u64, size: u64, inherit: bool) -> Result<()> {
        if address % PAGE != 0 || size % PAGE != 0 {
            return Err(Error::InvalidConfig("unaligned fork advice".into()));
        }
        if size == 0 {
            return Ok(());
        }
        let end = address.checked_add(size).ok_or(Error::ResourceLimit)?;
        if self.mapped_in_range(address, size) as u64 != size {
            return Err(Error::MemoryFault {
                address,
                access: Access::Read,
            });
        }
        for (_, page) in self.pages.range_mut(address / PAGE..end / PAGE) {
            page.inherit_on_fork = inherit;
        }
        Ok(())
    }
    pub fn mapped_in_range(&self, address: u64, size: u64) -> usize {
        if size == 0 {
            return 0;
        }
        let end = address.saturating_add(size - 1);
        self.pages.range(address / PAGE..=end / PAGE).count() * 4096
    }
    pub fn mark_bus_fault(&mut self, address: u64, size: u64) {
        if size == 0 {
            return;
        }
        let end = address.saturating_add(size - 1);
        for (_, page) in self.pages.range_mut(address / PAGE..=end / PAGE) {
            page.bus_fault = true;
        }
    }
    pub fn unmap(&mut self, address: u64, size: u64) -> Result<()> {
        if address % PAGE != 0 || size == 0 {
            return Err(Error::InvalidConfig("unaligned/empty unmap".into()));
        }
        let end = address.checked_add(size - 1).ok_or(Error::ResourceLimit)?;
        self.pages
            .retain(|page, _| *page < address / PAGE || *page > end / PAGE);
        Ok(())
    }
    pub fn protect(&mut self, address: u64, size: u64, permissions: Permissions) -> Result<()> {
        if address % PAGE != 0 {
            return Err(Error::InvalidConfig("unaligned protect".into()));
        }
        if size == 0 {
            return Ok(());
        }
        let end = address.checked_add(size - 1).ok_or(Error::ResourceLimit)?;
        let first = address / PAGE;
        let last = end / PAGE;
        if last - first >= self.max_pages as u64
            || (first..=last).any(|p| !self.pages.contains_key(&p))
        {
            return Err(Error::MemoryFault {
                address,
                access: Access::Write,
            });
        }
        for p in first..=last {
            self.pages.get_mut(&p).unwrap().permissions = permissions;
        }
        Ok(())
    }
    pub fn check(&self, address: u64, len: usize, access: Access) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let end = address
            .checked_add(len as u64 - 1)
            .ok_or(Error::MemoryFault { address, access })?;
        for p in address / PAGE..=end / PAGE {
            if !self
                .pages
                .get(&p)
                .is_some_and(|p| p.permissions.allows(access))
            {
                return Err(Error::MemoryFault {
                    address: address.max(p * PAGE),
                    access,
                });
            }
            if self.pages[&p].bus_fault {
                return Err(Error::BusFault {
                    address: address.max(p * PAGE),
                });
            }
        }
        Ok(())
    }
    pub fn read(&self, address: u64, dst: &mut [u8], access: Access) -> Result<()> {
        self.check(address, dst.len(), access)?;
        for (i, b) in dst.iter_mut().enumerate() {
            let a = address + i as u64;
            *b = self.pages[&(a / PAGE)].bytes[(a % PAGE) as usize];
        }
        Ok(())
    }
    pub fn write(&mut self, address: u64, src: &[u8]) -> Result<()> {
        self.check(address, src.len(), Access::Write)?;
        self.initialize(address, src)
    }
    /// Loader-only initialization; still checks all mapped addresses before mutating.
    pub fn initialize(&mut self, address: u64, src: &[u8]) -> Result<()> {
        if !src.is_empty() {
            let end = address
                .checked_add(src.len() as u64 - 1)
                .ok_or(Error::ResourceLimit)?;
            if (address / PAGE..=end / PAGE).any(|p| !self.pages.contains_key(&p)) {
                return Err(Error::MemoryFault {
                    address,
                    access: Access::Write,
                });
            }
        }
        for (i, b) in src.iter().enumerate() {
            let a = address + i as u64;
            self.pages.get_mut(&(a / PAGE)).unwrap().bytes[(a % PAGE) as usize] = *b;
        }
        Ok(())
    }
    pub fn fetch(&self, address: u64) -> Result<u8> {
        let mut b = [0];
        self.read(address, &mut b, Access::Execute)?;
        Ok(b[0])
    }
    pub fn load(&self, address: u64, width: usize) -> Result<u64> {
        let mut b = [0; 8];
        if !matches!(width, 1 | 2 | 4 | 8) {
            return Err(Error::InvalidConfig("integer width".into()));
        }
        self.read(address, &mut b[..width], Access::Read)?;
        Ok(u64::from_le_bytes(b))
    }
    pub fn store(&mut self, address: u64, value: u64, width: usize) -> Result<()> {
        if !matches!(width, 1 | 2 | 4 | 8) {
            return Err(Error::InvalidConfig("integer width".into()));
        }
        self.write(address, &value.to_le_bytes()[..width])
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Step {
    Continue,
    Syscall { number: u64, args: [u64; 6] },
}
pub trait Cpu {
    fn set_ticks(&mut self, _ticks: u64) {}
    fn step(&mut self, memory: &mut Memory) -> Result<Step>;
    fn set_result(&mut self, result: u64);
    fn pc(&self) -> u64;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cross_page_write_is_atomic() {
        let mut m = Memory::new(8192);
        m.map(0x1000, 4096, Permissions::RW).unwrap();
        m.store(0x1fff, 42, 1).unwrap();
        assert!(m.write(0x1fff, &[1, 2]).is_err());
        assert_eq!(m.load(0x1fff, 1).unwrap(), 42);
    }
    #[test]
    fn permissions_and_overflow() {
        let mut m = Memory::new(4096);
        m.map(0x1000, 4096, Permissions::RX).unwrap();
        assert!(m.write(0x1000, &[1]).is_err());
        assert!(m.check(u64::MAX, 2, Access::Read).is_err());
        assert!(m.map(0, 8192, Permissions::RW).is_err());
        assert_eq!(m.mapped_bytes(), 4096);
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::*;
    #[test]
    fn protection_change_is_atomic_and_unmap_tolerates_holes() {
        let mut m = Memory::new(12288);
        m.map(0x1000, 4096, Permissions::RW).unwrap();
        assert!(m.protect(0x1000, 8192, Permissions::RX).is_err());
        m.store(0x1000, 42, 8).unwrap();
        m.protect(0x1000, 4096, Permissions::RX).unwrap();
        assert!(m.store(0x1000, 3, 8).is_err());
        m.unmap(0x1000, 8192).unwrap();
        assert_eq!(m.mapped_bytes(), 0);
        assert!(m.load(0x1000, 1).is_err());
    }
}
