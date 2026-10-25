//! Small, explicit proc/device namespace. Contents describe guest state only.
use super::*;
use blink_linux::{VirtualContent, VirtualOpen};
use std::{
    io::Cursor,
    path::{Component, Path, PathBuf},
};

pub(super) fn path(cwd: &Path, name: &str) -> PathBuf {
    let source = if name.starts_with('/') {
        PathBuf::from(name)
    } else {
        cwd.join(name)
    };
    let mut result = PathBuf::from("/");
    for part in source.components() {
        match part {
            Component::Normal(n) => result.push(n),
            Component::ParentDir => {
                result.pop();
            }
            _ => (),
        }
    }
    result
}
pub(super) fn lookup(
    process: &Process,
    others: &BTreeMap<u32, Process>,
    name: &Path,
) -> Option<std::result::Result<VirtualOpen, u64>> {
    let name = name.to_str()?;
    if name != "/proc"
        && !name.starts_with("/proc/")
        && name != "/dev"
        && !name.starts_with("/dev/")
    {
        return None;
    }
    let mut stat = blink_host::FileStat {
        dev: 0xffff_fe00,
        nlink: 1,
        mode: 0o100444,
        blksize: 4096,
        ..Default::default()
    };
    let content = if name == "/dev/null"
        || name == "/dev/zero"
        || name == "/dev/random"
        || name == "/dev/urandom"
    {
        let minor = match name {
            "/dev/null" => 3,
            "/dev/zero" => 5,
            "/dev/random" => 8,
            _ => 9,
        };
        stat.ino = minor;
        stat.mode = 0o020666;
        stat.rdev = 256 | minor;
        if minor == 3 {
            VirtualContent::Null
        } else if minor == 5 {
            VirtualContent::Zero
        } else {
            VirtualContent::Random
        }
    } else if name == "/proc"
        || name == "/dev"
        || name == "/proc/self"
        || name == "/proc/thread-self"
        || name == format!("/proc/{}", process.group)
    {
        stat.mode = 0o040555;
        stat.ino = if name == "/dev" {
            100
        } else if name == "/proc" {
            200
        } else {
            1000 + process.group as u64
        };
        let mut entries = vec![(b".".to_vec(), 4), (b"..".to_vec(), 4)];
        if name == "/dev" {
            for item in ["null", "zero", "random", "urandom"] {
                entries.push((item.as_bytes().to_vec(), 2));
            }
        } else if name == "/proc" {
            for (item, kind) in [
                ("self".to_owned(), 10),
                ("thread-self".to_owned(), 10),
                ("mounts".to_owned(), 8),
                (process.group.to_string(), 4),
            ] {
                entries.push((item.into_bytes(), kind));
            }
        } else {
            for item in ["status", "cmdline", "environ", "mounts"] {
                entries.push((item.as_bytes().to_vec(), 8));
            }
        }
        VirtualContent::Directory {
            entries: entries
                .into_iter()
                .enumerate()
                .map(|(n, (name, kind))| blink_host::DirectoryEntry {
                    inode: stat.ino + n as u64,
                    kind,
                    name,
                })
                .collect(),
            position: 0,
        }
    } else {
        let own = format!("/proc/{}/", process.group);
        let leaf = name
            .strip_prefix("/proc/self/")
            .or_else(|| name.strip_prefix("/proc/thread-self/"))
            .or_else(|| name.strip_prefix(&own));
        let bytes = match leaf {
            Some("status") => {
                let threads = others.values().filter(|p| p.group == process.group).count() + 1;
                let command = process
                    .argv
                    .first()
                    .and_then(|p| Path::new(p).file_name())
                    .map(|p| p.to_string_lossy())
                    .unwrap_or_default();
                let c = &process.credentials;
                let (effective, permitted) = super::credentials::capabilities(c);
                format!("Name:\t{}\nState:\tR (running)\nTgid:\t{}\nPid:\t{}\nPPid:\t{}\nUid:\t{}\t{}\t{}\t{}\nGid:\t{}\t{}\t{}\t{}\nThreads:\t{}\nVmSize:\t{} kB\nSigPnd:\t{:016x}\nSigBlk:\t{:016x}\nCapInh:\t0000000000000000\nCapPrm:\t{permitted:016x}\nCapEff:\t{effective:016x}\n",command,process.group,process.group,process.parent,c.uid,c.euid,c.suid,c.euid,c.gid,c.egid,c.sgid,c.egid,threads,process.memory.borrow().mapped_bytes()/1024,process.signals.bits,process.signal_mask).into_bytes()
            }
            Some("cmdline") => nul_strings(&process.argv),
            Some("environ") => nul_strings(&process.env),
            Some("mounts") => mounts(),
            _ if name == "/proc/mounts" => mounts(),
            _ => return Some(Err(errno(2))),
        };
        stat.ino = 2000 + process.group as u64;
        VirtualContent::Bytes(Cursor::new(bytes))
    };
    Some(Ok(VirtualOpen {
        path: PathBuf::from(name),
        stat,
        content,
        read: true,
        write: false,
    }))
}
fn nul_strings(strings: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for s in strings {
        bytes.extend(s.as_bytes());
        bytes.push(0);
    }
    bytes
}
fn mounts() -> Vec<u8> {
    b"rootfs / rootfs rw,nosuid,nodev 0 0\nproc /proc proc ro,nosuid,nodev,noexec 0 0\ndevfs /dev devfs rw,nosuid,noexec 0 0\n".to_vec()
}
pub(super) fn open(mut node: VirtualOpen, flags: u64) -> std::result::Result<Fd, u64> {
    let directory = matches!(node.content, VirtualContent::Directory { .. });
    if flags & 0x10000 != 0 && !directory {
        return Err(errno(20));
    }
    if flags & 128 != 0 && flags & 64 != 0 {
        return Err(errno(17));
    }
    if flags & 3 == 3 {
        return Err(errno(22));
    }
    let writable = matches!(node.content, VirtualContent::Null | VirtualContent::Zero);
    if flags & 3 != 0 && !writable {
        return Err(errno(if directory { 21 } else { 13 }));
    }
    if flags & !(3 | 64 | 512 | 1024 | 0x800 | 0x8000 | 0x10000 | 0x20000 | 0x80000) != 0 {
        return Err(errno(22));
    }
    node.read = flags & 3 != 1;
    node.write = flags & 3 != 0;
    Ok(Fd::Virtual(Rc::new(RefCell::new(node))))
}

pub(super) fn filesystem(path: &Path) -> Option<blink_host::FilesystemStat> {
    let (kind, readonly) = if path.starts_with("/proc") {
        (0x9fa0, true)
    } else if path.starts_with("/dev") {
        (0x01021994, false)
    } else {
        return None;
    };
    Some(blink_host::FilesystemStat {
        kind,
        block_size: 4096,
        fragment_size: 4096,
        blocks: 0,
        free_blocks: 0,
        available_blocks: 0,
        files: 0,
        free_files: 0,
        id: kind,
        name_max: 255,
        readonly,
    })
}

pub(super) fn link_target(process: &Process, path: &Path) -> Option<PathBuf> {
    if path == Path::new("/proc/self") {
        Some(process.group.to_string().into())
    } else if path == Path::new("/proc/thread-self") {
        Some(format!("{}/task/{}", process.group, process.tid).into())
    } else if path == Path::new("/proc/self/exe")
        || path == Path::new("/proc/thread-self/exe")
        || path == Path::new(&format!("/proc/{}/exe", process.group))
    {
        Some(process.executable.clone())
    } else {
        None
    }
}
pub(super) fn stat(
    config: &RuntimeConfig,
    process: &Process,
    others: &BTreeMap<u32, Process>,
    path: &Path,
    nofollow: bool,
) -> Option<std::result::Result<blink_host::FileStat, u64>> {
    if let Some(target) = link_target(process, path) {
        if nofollow {
            return Some(Ok(blink_host::FileStat {
                dev: 0xffff_fe00,
                ino: 3000 + process.group as u64,
                nlink: 1,
                mode: 0o120777,
                size: target.as_os_str().as_encoded_bytes().len() as u64,
                blksize: 4096,
                ..Default::default()
            }));
        }
        if target.is_absolute() {
            return Some(
                blink_host::path_stat(
                    &config.rootfs,
                    Path::new("/"),
                    &target,
                    false,
                    config.rootless_metadata,
                )
                .map_err(|e| blink_linux::errno(&e)),
            );
        }
    }
    lookup(process, others, path).map(|node| node.map(|node| node.stat))
}
