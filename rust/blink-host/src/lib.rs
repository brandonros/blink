//! Host operations kept separate from guest ABI layouts.
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
