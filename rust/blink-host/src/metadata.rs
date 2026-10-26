//! Persistent rootless build metadata. Compatible with the C backend's Darwin
//! attribute payloads; Linux requires the `user.` attribute namespace.
//! This is privileged build bookkeeping, not a guest permission/capability model.
use crate::{file_stat, FileStat};
use std::{fs, io, path::Path};
use xattr::FileExt;
#[cfg(target_os = "linux")]
const OWNER: &str = "user.org.image-forge.owner.v1";
#[cfg(not(target_os = "linux"))]
const OWNER: &str = "org.image-forge.owner.v1";
#[cfg(target_os = "linux")]
const MODE: &str = "user.org.image-forge.mode.v1";
#[cfg(not(target_os = "linux"))]
const MODE: &str = "org.image-forge.mode.v1";

#[derive(Clone, Copy)]
pub enum Target<'a> {
    Path(&'a Path),
    File(&'a fs::File),
}
impl Target<'_> {
    fn get(self, key: &str) -> io::Result<Option<Vec<u8>>> {
        match self {
            Self::Path(p) => xattr::get(p, key),
            Self::File(f) => f.get_xattr(key),
        }
    }
    fn set(self, key: &str, value: &[u8]) -> io::Result<()> {
        match self {
            Self::Path(p) => xattr::set(p, key, value),
            Self::File(f) => f.set_xattr(key, value),
        }
    }
    pub fn stat(self, rootless: bool) -> io::Result<FileStat> {
        let metadata = match self {
            Self::Path(p) => fs::symlink_metadata(p)?,
            Self::File(f) => f.metadata()?,
        };
        let mut stat = file_stat(&metadata)?;
        if rootless {
            stat.uid = 0;
            stat.gid = 0;
            if stat.mode & 0o170000 == 0o120000 {
                stat.mode = (stat.mode & 0o170000) | 0o777;
            }
            if let Some(owner) = self.get(OWNER)? {
                if owner.len() != 8 {
                    return Err(io::Error::other("invalid rootless owner attribute"));
                }
                stat.uid = u32::from_le_bytes(owner[..4].try_into().unwrap());
                stat.gid = u32::from_le_bytes(owner[4..].try_into().unwrap());
            }
            if let Some(mode) = self.get(MODE)? {
                if mode.len() != 4 {
                    return Err(io::Error::other("invalid rootless mode attribute"));
                }
                stat.mode = (stat.mode & 0o170000)
                    | (u32::from_le_bytes(mode.try_into().unwrap()) & 0o7777);
            }
        }
        Ok(stat)
    }
    pub fn chmod(self, mode: u32, rootless: bool) -> io::Result<()> {
        let stat = self.stat(rootless)?;
        if stat.mode & 0o170000 == 0o120000 {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let host_mode = if rootless {
            (mode & 0o777)
                | if stat.mode & 0o170000 == 0o040000 {
                    0o700
                } else {
                    0o600
                }
        } else {
            mode
        };
        match self {
            Self::Path(p) => crate::chmod(p, host_mode)?,
            Self::File(f) => crate::fchmod(f, host_mode)?,
        }
        if rootless {
            self.set(MODE, &(mode & 0o7777).to_le_bytes())?;
        }
        Ok(())
    }
    pub fn chown(self, uid: u32, gid: u32, rootless: bool) -> io::Result<()> {
        if !rootless {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let stat = self.stat(true)?;
        let uid = if uid == u32::MAX { stat.uid } else { uid };
        let gid = if gid == u32::MAX { stat.gid } else { gid };
        if stat.mode & 0o170000 == 0o120000 && uid == stat.uid && gid == stat.gid {
            return Ok(()); // No metadata change is needed, including on Linux symlinks.
        }
        let mut owner = [0; 8];
        owner[..4].copy_from_slice(&uid.to_le_bytes());
        owner[4..].copy_from_slice(&gid.to_le_bytes());
        self.set(OWNER, &owner)?;
        // Linux chown drops SUID and executable SGID on non-directories.
        if stat.mode & 0o170000 != 0o040000 && stat.mode & 0o170000 != 0o120000 {
            let mode = stat.mode
                & !0o4000
                & if stat.mode & 0o10 != 0 {
                    !0o2000
                } else {
                    u32::MAX
                };
            if mode != stat.mode {
                self.chmod(mode, true)?;
            }
        }
        Ok(())
    }
    pub fn created(self, parent: &Path, uid: u32, gid: u32, mode: u32) -> io::Result<()> {
        let parent = Target::Path(parent).stat(true)?;
        let directory = self.stat(false)?.mode & 0o170000 == 0o040000;
        self.chmod(mode | if directory { 0o700 } else { 0o600 }, false)?;
        self.chown(
            uid,
            if parent.mode & 0o2000 != 0 {
                parent.gid
            } else {
                gid
            },
            true,
        )?;
        self.chmod(
            mode | if directory && parent.mode & 0o2000 != 0 {
                0o2000
            } else {
                0
            },
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_modes_follow_inode_without_changing_host_owner() {
        let root = std::env::temp_dir().join(format!("blink-metadata-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("original");
        fs::write(&path, b"persistent").unwrap();
        let file = fs::File::open(&path).unwrap();
        let before = file_stat(&file.metadata().unwrap()).unwrap();
        let target = Target::File(&file);
        target.created(&root, 123, 456, 0o6750).unwrap();
        let stat = target.stat(true).unwrap();
        assert_eq!((stat.uid, stat.gid, stat.mode & 0o7777), (123, 456, 0o6750));
        fs::hard_link(&path, root.join("hard")).unwrap();
        fs::rename(&path, root.join("renamed")).unwrap();
        let hard = root.join("hard");
        Target::Path(&hard).chown(222, u32::MAX, true).unwrap();
        let stat = target.stat(true).unwrap();
        assert_eq!((stat.uid, stat.gid, stat.mode & 0o7777), (222, 456, 0o750));
        target.chmod(0o2640, true).unwrap();
        target.chown(333, 444, true).unwrap();
        assert_eq!(target.stat(true).unwrap().mode & 0o7777, 0o2640);
        target.chmod(0, true).unwrap();
        assert_eq!(target.stat(true).unwrap().mode & 0o7777, 0);
        assert_eq!(fs::read(&hard).unwrap(), b"persistent");
        let after = file_stat(&file.metadata().unwrap()).unwrap();
        assert_eq!((before.uid, before.gid), (after.uid, after.gid));
        assert_eq!(after.mode & 0o600, 0o600);
        drop(file);
        let reopened = fs::File::open(&hard).unwrap();
        assert_eq!(Target::File(&reopened).stat(true).unwrap().uid, 333);
        // Attribute payload corruption must be reported, never reset to root.
        reopened.set_xattr(OWNER, b"bad").unwrap();
        assert!(Target::File(&reopened).stat(true).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn setgid_directory_inheritance_is_guest_metadata() {
        let root = std::env::temp_dir().join(format!("blink-metadata-sgid-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Target::Path(&root).chown(10, 20, true).unwrap();
        Target::Path(&root).chmod(0o2750, true).unwrap();
        let child = root.join("child");
        fs::create_dir(&child).unwrap();
        Target::Path(&child).created(&root, 30, 40, 0o700).unwrap();
        let stat = Target::Path(&child).stat(true).unwrap();
        assert_eq!((stat.uid, stat.gid, stat.mode & 0o7777), (30, 20, 0o2700));
        fs::remove_dir_all(root).unwrap();
    }
}
