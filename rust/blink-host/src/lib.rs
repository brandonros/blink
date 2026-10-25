//! Host operations kept separate from guest ABI layouts.
use std::{
    fs, io,
    path::{Path, PathBuf},
};
/// Rooted lookup is build bookkeeping, not a security boundary against host races.
pub fn resolve(root: &Path, guest: &Path) -> io::Result<PathBuf> {
    let mut relative = PathBuf::new();
    for c in guest.components() {
        match c {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::Normal(v) => relative.push(v),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "guest path traversal",
                ))
            }
        }
    }
    let path = fs::canonicalize(root.join(relative))?;
    if !path.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "path leaves root",
        ));
    }
    Ok(path)
}
