//! Host operations kept separate from guest ABI layouts.
pub mod metadata;
use std::{
    collections::VecDeque,
    fs, io,
    path::{Component, Path, PathBuf},
};
/// Resolve Linux-style paths and symlinks under a root. Host pathname races mean
/// this is build bookkeeping, not a security boundary.
pub fn resolve(root: &Path, guest: &Path) -> io::Result<PathBuf> {
    resolve_from(root, Path::new("/"), guest, false)
}
pub fn resolve_from(
    root: &Path,
    cwd: &Path,
    guest: &Path,
    missing_final: bool,
) -> io::Result<PathBuf> {
    if guest.as_os_str().is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "empty guest path"));
    }
    let mut current = PathBuf::new();
    if !guest.is_absolute() {
        for c in cwd.components() {
            if let Component::Normal(n) = c {
                current.push(n);
            }
        }
    }
    let mut parts: VecDeque<_> = guest
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    let mut links = 0;
    while let Some(part) = parts.pop_front() {
        if part == "/" {
            current.clear();
            continue;
        }
        if part == "." {
            continue;
        }
        if part == ".." {
            current.pop();
            continue;
        }
        let candidate = root.join(&current).join(&part);
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    links += 1;
                    if links > 40 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "too many symbolic links",
                        ));
                    }
                    let target = fs::read_link(&candidate)?;
                    for c in target.components().rev() {
                        parts.push_front(c.as_os_str().to_owned());
                    }
                } else {
                    current.push(part);
                    if !parts.is_empty() && !metadata.is_dir() {
                        return Err(io::Error::new(
                            io::ErrorKind::NotADirectory,
                            "path component is not a directory",
                        ));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && missing_final && parts.is_empty() => {
                current.push(part)
            }
            Err(e) => return Err(e),
        }
    }
    let path = root.join(current);
    let raw = guest.as_os_str().as_encoded_bytes();
    if (raw.ends_with(b"/") || raw.ends_with(b"/.")) && !path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "trailing directory component",
        ));
    }
    Ok(path)
}
pub fn fill_random(bytes: &mut [u8]) -> io::Result<()> {
    use std::io::Read;
    #[cfg(unix)]
    {
        fs::File::open("/dev/urandom")?.read_exact(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = bytes;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "host entropy source not implemented",
        ))
    }
}

pub fn read_link(root: &Path, cwd: &Path, guest: &Path) -> io::Result<PathBuf> {
    let name = guest
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a symlink"))?;
    let parent = guest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::read_link(resolve_from(root, cwd, parent, false)?.join(name))
}

#[derive(Default)]
pub struct FileStat {
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub size: u64,
    pub blksize: u64,
    pub blocks: u64,
    pub atime: i64,
    pub atime_nsec: i64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub ctime: i64,
    pub ctime_nsec: i64,
}
pub fn file_stat(metadata: &fs::Metadata) -> io::Result<FileStat> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(FileStat {
            dev: metadata.dev(),
            ino: metadata.ino(),
            nlink: metadata.nlink(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            rdev: metadata.rdev(),
            size: metadata.size(),
            blksize: metadata.blksize(),
            blocks: metadata.blocks(),
            atime: metadata.atime(),
            atime_nsec: metadata.atime_nsec(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err(io::Error::new(io::ErrorKind::Unsupported, "host metadata"))
    }
}

pub fn read_at(file: &fs::File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_at(bytes, offset)
    }
    #[cfg(not(unix))]
    {
        let _ = (file, bytes, offset);
        Err(io::Error::new(io::ErrorKind::Unsupported, "pread"))
    }
}

pub fn path_stat(
    root: &Path,
    cwd: &Path,
    guest: &Path,
    nofollow: bool,
    rootless: bool,
) -> io::Result<FileStat> {
    let path = if nofollow {
        if let Some(name) = guest.file_name() {
            let parent = guest
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            resolve_from(root, cwd, parent, false)?.join(name)
        } else {
            resolve_from(root, cwd, guest, false)?
        }
    } else {
        resolve_from(root, cwd, guest, false)?
    };
    metadata::Target::Path(&path).stat(rootless)
}

/// Resolve a directory entry without following its final symlink. Used for
/// unlink/rename/link creation; never remove the context root through `/` or `..`.
pub fn entry_path(root: &Path, cwd: &Path, guest: &Path) -> io::Result<PathBuf> {
    let raw = guest.as_os_str().as_encoded_bytes();
    let trimmed = raw.strip_suffix(b"/").unwrap_or(raw);
    let leaf = trimmed.rsplit(|b| *b == b'/').next().unwrap_or_default();
    if leaf.is_empty() || leaf == b"." || leaf == b".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid mutation entry",
        ));
    }
    let name = guest
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing entry"))?;
    let parent = guest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(resolve_from(root, cwd, parent, false)?.join(name))
}
pub fn mkdir(path: &Path, mode: u32) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(mode & 0o7777);
    }
    #[cfg(not(unix))]
    let _ = mode;
    builder.create(path)
}
pub fn chmod(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Err(io::ErrorKind::Unsupported.into())
    }
}
pub fn fchmod(file: &fs::File, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode & 0o7777))
    }
    #[cfg(not(unix))]
    {
        let _ = (file, mode);
        Err(io::ErrorKind::Unsupported.into())
    }
}
pub fn symlink(target: &Path, path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, path)
    }
    #[cfg(not(unix))]
    {
        let _ = (target, path);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[derive(Clone)]
pub struct DirectoryEntry {
    pub inode: u64,
    pub kind: u8,
    pub name: Vec<u8>,
}
pub struct Directory {
    pub file: fs::File,
    pub path: PathBuf,
    root: PathBuf,
    pub position: u64,
    entries: fs::ReadDir,
    pending: VecDeque<DirectoryEntry>,
}
impl Directory {
    pub fn open(path: PathBuf, root: PathBuf) -> io::Result<Self> {
        let file = fs::File::open(&path)?;
        let inode = file_stat(&file.metadata()?)?.ino;
        let parent = file_stat(&fs::metadata(if path == root {
            &path
        } else {
            path.parent().unwrap_or(&path)
        })?)?
        .ino;
        let pending = VecDeque::from([
            DirectoryEntry {
                inode,
                kind: 4,
                name: b".".to_vec(),
            },
            DirectoryEntry {
                inode: parent,
                kind: 4,
                name: b"..".to_vec(),
            },
        ]);
        Ok(Self {
            entries: fs::read_dir(&path)?,
            file,
            path,
            root,
            position: 0,
            pending,
        })
    }
    pub fn peek(&mut self) -> io::Result<Option<DirectoryEntry>> {
        if self.pending.is_empty() {
            if let Some(entry) = self.entries.next() {
                let entry = entry?;
                let stat = file_stat(&fs::symlink_metadata(entry.path())?)?;
                self.pending.push_back(DirectoryEntry {
                    inode: stat.ino,
                    kind: ((stat.mode >> 12) & 15) as u8,
                    name: entry.file_name().as_encoded_bytes().to_vec(),
                });
            }
        }
        Ok(self.pending.front().cloned())
    }
    pub fn advance(&mut self) {
        self.pending.pop_front();
        self.position += 1;
    }
    pub fn rewind(&mut self) -> io::Result<()> {
        *self = Self::open(self.path.clone(), self.root.clone())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_symlink_is_guest_rooted() {
        let root = std::env::temp_dir().join(format!("blink-host-{}", std::process::id()));
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("real/file"), b"ok").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/real", root.join("link")).unwrap();
            assert_eq!(
                resolve(&root, Path::new("/link/file")).unwrap(),
                root.join("real/file")
            );
        }
        assert_eq!(
            resolve(&root, Path::new("/../../real/file")).unwrap(),
            root.join("real/file")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
