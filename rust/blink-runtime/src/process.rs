//! Cooperative guest processes. No host fork, host signals, or blocking guest pipes.
use crate::{elf, Command, RunOutcome, RuntimeConfig, Termination};
use blink_core::{Access, Cpu, Error, GuestArch, Memory, Permissions, Result, Step};
use blink_linux::{Fd, Pipe, SignalAction, UNBLOCKABLE};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{Read, Seek, SeekFrom, Write},
    rc::Rc,
    time::Instant,
};
#[derive(Clone)]
enum Engine {
    X86(blink_x86_64::X86),
    Arm(blink_aarch64::Aarch64),
}
impl Engine {
    fn new(arch: GuestArch, entry: u64, stack: u64) -> Self {
        match arch {
            GuestArch::X86_64 => Self::X86(blink_x86_64::X86::new(entry, stack)),
            GuestArch::Aarch64 => Self::Arm(blink_aarch64::Aarch64::new(entry, stack)),
        }
    }
    fn stack(&mut self, value: u64) {
        match self {
            Self::X86(c) => c.regs[4] = value,
            Self::Arm(c) => c.sp = value,
        }
    }
    fn tls(&mut self, value: u64) {
        match self {
            Self::X86(c) => c.fs_base = value,
            Self::Arm(c) => c.tpidr = value,
        }
    }
    fn cpu(&mut self) -> &mut dyn Cpu {
        match self {
            Self::X86(c) => c,
            Self::Arm(c) => c,
        }
    }
}
#[derive(Clone)]
struct Heap {
    base: u64,
    brk: u64,
    next_map: u64,
}
impl Heap {
    fn new(base: u64) -> Self {
        Self {
            base,
            brk: base,
            next_map: 0x100000000,
        }
    }
}
#[derive(Clone)]
struct Process {
    parent: u32,
    credentials: blink_linux::Credentials,
    executable: std::path::PathBuf,
    cpu: Engine,
    memory: Rc<RefCell<Memory>>,
    heap: Rc<RefCell<Heap>>,
    fds: Rc<RefCell<BTreeMap<i32, Fd>>>,
    descriptor_flags: Rc<RefCell<BTreeSet<i32>>>,
    group: u32,
    cwd: Rc<RefCell<std::path::PathBuf>>,
    clear_tid: u64,
    robust_head: u64,
    actions: Rc<RefCell<[SignalAction; 64]>>,
    signal_mask: u64,
    pending_signals: u64,
    signal_senders: [u32; 64],
    futex: Option<u64>,
    poll_deadline: Option<Instant>,
    pending: Option<(u64, [u64; 6])>,
}
impl Process {
    fn fd(&self) -> Option<i32> {
        (0..1024).find(|fd| !self.fds.borrow().contains_key(fd))
    }
}
struct Zombie {
    parent: u32,
    status: i32,
}
enum Event {
    Ready,
    Blocked,
    Exit(i32),
    Signal(i32),
}
fn errno(n: i64) -> u64 {
    (-n) as u64
}
fn cstring(memory: &Memory, ptr: u64) -> Result<String> {
    let mut bytes = Vec::new();
    for i in 0..4096 {
        let b = memory.load(ptr.checked_add(i).ok_or(Error::ResourceLimit)?, 1)? as u8;
        if b == 0 {
            return String::from_utf8(bytes)
                .map_err(|_| Error::InvalidConfig("non-UTF8 exec string".into()));
        }
        bytes.push(b);
    }
    Err(Error::ResourceLimit)
}
fn strings(memory: &Memory, ptr: u64) -> Result<Vec<String>> {
    let mut values = Vec::new();
    if ptr == 0 {
        return Ok(values);
    }
    for i in 0..4096 {
        let p = memory.load(ptr.checked_add(i * 8).ok_or(Error::ResourceLimit)?, 8)?;
        if p == 0 {
            return Ok(values);
        }
        values.push(cstring(memory, p)?);
    }
    Err(Error::ResourceLimit)
}
fn image(
    config: &RuntimeConfig,
    path: &std::path::Path,
    cwd: &std::path::Path,
    args: &[String],
    env: &[String],
    credentials: &blink_linux::Credentials,
) -> Result<elf::Image> {
    let bytes = read_image(config, cwd, path)?;
    let interp = elf::interpreter(&bytes)?
        .map(|path| read_image(config, cwd, std::path::Path::new(&path)))
        .transpose()?;
    elf::load(
        &bytes,
        interp.as_deref(),
        config.guest_arch,
        config.max_memory,
        args,
        env,
        credentials,
    )
}
fn read_image(
    config: &RuntimeConfig,
    cwd: &std::path::Path,
    path: &std::path::Path,
) -> Result<Vec<u8>> {
    let path = blink_host::resolve_from(&config.rootfs, cwd, path, false)?;
    if !std::fs::metadata(&path)?.is_file() {
        return Err(Error::InvalidConfig("program must be regular".into()));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(config.max_image_size as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > config.max_image_size {
        return Err(Error::ResourceLimit);
    }
    Ok(bytes)
}

pub(super) fn run(
    config: &RuntimeConfig,
    command: Command,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<RunOutcome> {
    let start = Instant::now();
    let mut args = vec![command.program.to_string_lossy().into_owned()];
    args.extend(command.args);
    let cwd_path = blink_host::resolve(&config.rootfs, &command.cwd)?;
    if !cwd_path.is_dir() {
        return Err(Error::InvalidConfig("cwd must be a directory".into()));
    }
    let cwd = std::path::Path::new("/").join(
        cwd_path
            .strip_prefix(&config.rootfs)
            .map_err(|_| Error::InvalidConfig("cwd outside root".into()))?,
    );
    let credentials = blink_linux::Credentials::new(command.uid, command.gid);
    let initial = image(
        config,
        &command.program,
        &cwd,
        &args,
        &command.env,
        &credentials,
    )?;
    let mut processes = BTreeMap::from([(
        1u32,
        Process {
            parent: 0,
            credentials,
            executable: guest_executable(config, &cwd, &command.program)?,
            cpu: Engine::new(config.guest_arch, initial.entry, initial.stack),
            memory: Rc::new(RefCell::new(initial.memory)),
            heap: Rc::new(RefCell::new(Heap::new(initial.heap))),
            group: 1,
            cwd: Rc::new(RefCell::new(cwd)),
            clear_tid: 0,
            robust_head: 0,
            actions: Rc::new(RefCell::new([SignalAction::default(); 64])),
            signal_mask: 0,
            pending_signals: 0,
            signal_senders: [0; 64],
            futex: None,
            poll_deadline: None,
            fds: Rc::new(RefCell::new(BTreeMap::from([
                (0, Fd::Stdin),
                (1, Fd::Stdout),
                (2, Fd::Stderr),
            ]))),
            pending: None,
            descriptor_flags: Rc::new(RefCell::new(BTreeSet::new())),
        },
    )]);
    let mut zombies = BTreeMap::<u32, Zombie>::new();
    let mut group_status = BTreeMap::<u32, i32>::new();
    let mut next_pid = 2u32;
    let mut next_pipe = 16;
    let mut queue = VecDeque::from([1u32]);
    let mut instructions = 0;
    let termination = 'scheduler: loop {
        if command.cancellation.is_cancelled() {
            break Termination::Cancelled;
        }
        if command.timeout.is_some_and(|d| start.elapsed() >= d) {
            break Termination::TimedOut;
        }
        if instructions >= config.max_instructions {
            break Termination::InstructionLimit;
        }
        let pid = queue
            .pop_front()
            .ok_or_else(|| Error::InvalidConfig("empty process scheduler".into()))?;
        let Some(mut process) = processes.remove(&pid) else {
            continue;
        };
        let mut event = Event::Ready;
        // A quantum bounds cancellation latency and keeps runnable guests fair.
        for _ in 0..256 {
            if let Some(signal) = deliver_signal(&mut process)? {
                event = Event::Signal(signal);
                break;
            }
            if process.futex.is_some() {
                event = Event::Blocked;
                break;
            }
            if instructions >= config.max_instructions {
                break;
            }
            let syscall = if let Some(call) = process.pending.take() {
                Some(call)
            } else {
                instructions += 1;
                process.cpu.cpu().set_ticks(instructions);
                let pc = process.cpu.cpu().pc();
                match process.cpu.cpu().step(&mut process.memory.borrow_mut()) {
                    Ok(Step::Continue) => None,
                    Ok(Step::Syscall { number, args }) => {
                        if config.trace {
                            writeln!(stderr, "[guest {pid} {pc:#x}] syscall {number} {args:x?}")?;
                        }
                        Some((number, args))
                    }
                    Err(Error::BusFault { .. }) => {
                        event = Event::Signal(7);
                        break;
                    }
                    Err(Error::ArithmeticFault) => {
                        event = Event::Signal(8);
                        break;
                    }
                    Err(Error::MemoryFault { address, access }) => {
                        if config.trace {
                            writeln!(
                                stderr,
                                "[guest {pid} {pc:#x}] {access:?} fault at {address:#x}"
                            )?;
                        }
                        event = Event::Signal(11);
                        break;
                    }
                    Err(e) => return Err(e),
                }
            };
            if let Some((number, args)) = syscall {
                // Normalize architecture-specific syscall numbers at this boundary.
                let nr = match config.guest_arch {
                    GuestArch::X86_64 => number,
                    GuestArch::Aarch64 => match number {
                        63 => 0,
                        25 => 72,
                        160 => 63,
                        174 => 102,
                        175 => 107,
                        176 => 104,
                        177 => 108,
                        146 => 105,
                        144 => 106,
                        148 => 118,
                        150 => 120,
                        158 => 115,
                        159 => 116,
                        71 => 40,
                        29 => 16,
                        66 => 20,
                        67 => 17,
                        79 => 262,
                        48 => 269,
                        80 => 5,
                        78 => 267,
                        99 => 273,
                        261 => 302,
                        278 => 318,
                        293 => 334,
                        214 => 12,
                        215 => 11,
                        222 => 9,
                        226 => 10,
                        56 => 257,
                        62 => 8,
                        17 => 79,
                        49 => 80,
                        64 => 1,
                        57 => 3,
                        59 => 293,
                        23 => 32,
                        24 => 292,
                        220 => 56,
                        221 => 59,
                        260 => 61,
                        172 => 39,
                        178 => 186,
                        96 => 218,
                        98 => 202,
                        173 => 110,
                        129 => 62,
                        93 => 60,
                        94 => 231,
                        _ => u64::MAX,
                    },
                };
                let ret = match nr {
                    60 | 231 => {
                        if nr == 231 {
                            processes.retain(|_, p| p.group != process.group);
                            group_status.remove(&process.group);
                        }
                        event = Event::Exit((args[0] & 255) as i32);
                        break;
                    }
                    63 => {
                        let machine = if config.guest_arch == GuestArch::X86_64 {
                            "x86_64"
                        } else {
                            "aarch64"
                        };
                        let fields = [
                            "Linux",
                            "blink",
                            "6.0.0-blink-rs",
                            "Rust userspace interpreter",
                            machine,
                            "localdomain",
                        ];
                        let mut bytes = [0u8; 390];
                        for (n, value) in fields.iter().enumerate() {
                            bytes[n * 65..n * 65 + value.len()].copy_from_slice(value.as_bytes());
                        }
                        if process.memory.borrow_mut().write(args[0], &bytes).is_err() {
                            errno(14)
                        } else {
                            0
                        }
                    }
                    102 => process.credentials.uid as u64,
                    107 => process.credentials.euid as u64,
                    104 => process.credentials.gid as u64,
                    108 => process.credentials.egid as u64,
                    105 | 106 => {
                        let id = args[0] as u32;
                        let c = &mut process.credentials;
                        if id == u32::MAX {
                            errno(22)
                        } else if nr == 105 {
                            if c.euid == 0 {
                                c.uid = id;
                                c.euid = id;
                                c.suid = id;
                                0
                            } else if id == c.uid || id == c.suid {
                                c.euid = id;
                                0
                            } else {
                                errno(1)
                            }
                        } else if c.euid == 0 {
                            c.gid = id;
                            c.egid = id;
                            c.sgid = id;
                            0
                        } else if id == c.gid || id == c.sgid {
                            c.egid = id;
                            0
                        } else {
                            errno(1)
                        }
                    }
                    118 | 120 => {
                        if args[..3]
                            .iter()
                            .any(|p| process.memory.borrow().check(*p, 4, Access::Write).is_err())
                        {
                            errno(14)
                        } else {
                            let c = &process.credentials;
                            let ids = if nr == 118 {
                                [c.uid, c.euid, c.suid]
                            } else {
                                [c.gid, c.egid, c.sgid]
                            };
                            for n in 0..3 {
                                process
                                    .memory
                                    .borrow_mut()
                                    .store(args[n], ids[n] as u64, 4)?;
                            }
                            0
                        }
                    }
                    115 => {
                        let groups = &process.credentials.groups;
                        if args[0] == 0 {
                            groups.len() as u64
                        } else if args[0] < groups.len() as u64 {
                            errno(22)
                        } else if process
                            .memory
                            .borrow()
                            .check(args[1], groups.len() * 4, Access::Write)
                            .is_err()
                        {
                            errno(14)
                        } else {
                            for (n, gid) in groups.iter().enumerate() {
                                process.memory.borrow_mut().store(
                                    args[1] + n as u64 * 4,
                                    *gid as u64,
                                    4,
                                )?;
                            }
                            groups.len() as u64
                        }
                    }
                    116 => {
                        if process.credentials.euid != 0 {
                            errno(1)
                        } else if args[0] > 65536 {
                            errno(22)
                        } else if process
                            .memory
                            .borrow()
                            .check(args[1], args[0] as usize * 4, Access::Read)
                            .is_err()
                        {
                            errno(14)
                        } else {
                            let mut groups = Vec::new();
                            for n in 0..args[0] {
                                groups
                                    .push(process.memory.borrow().load(args[1] + n * 4, 4)? as u32);
                            }
                            process.credentials.groups = groups;
                            0
                        }
                    }
                    21 | 269 | 439 => {
                        let (ptr, mode) = if nr == 21 {
                            (args[0], args[1])
                        } else {
                            (args[1], args[2])
                        };
                        let path = cstring(&process.memory.borrow(), ptr);
                        match path {
                            Err(_) => errno(14),
                            Ok(path) => {
                                if (nr != 21 && args[0] as i32 != -100 && !path.starts_with('/'))
                                    || (nr == 439 && args[3] != 0)
                                {
                                    return Err(Error::UnsupportedSyscall(number));
                                }
                                if mode & !7 != 0 {
                                    errno(22)
                                } else {
                                    match blink_host::path_stat(
                                        &config.rootfs,
                                        &process.cwd.borrow(),
                                        std::path::Path::new(&path),
                                        false,
                                    ) {
                                        Err(e) => blink_linux::errno(&e),
                                        Ok(stat) => {
                                            if (mode & 1 != 0 && stat.mode & 0o111 == 0)
                                                || (mode & 2 != 0 && stat.mode & 0o222 == 0)
                                                || (mode & 4 != 0 && stat.mode & 0o444 == 0)
                                            {
                                                errno(13)
                                            } else {
                                                0
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    4 | 6 | 262 => {
                        let (ptr, buf, flags) = if nr == 262 {
                            (args[1], args[2], args[3])
                        } else {
                            (args[0], args[1], if nr == 6 { 256 } else { 0 })
                        };
                        if flags & !(256 | 4096) != 0 {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        let path = cstring(&process.memory.borrow(), ptr);
                        match path {
                            Err(_) => errno(14),
                            Ok(path) => {
                                let stat = if nr == 262
                                    && flags & 4096 != 0
                                    && path.is_empty()
                                    && args[0] as i32 != -100
                                {
                                    process
                                        .fds
                                        .borrow()
                                        .get(&(args[0] as i32))
                                        .map(|fd| fd.stat())
                                } else {
                                    if nr == 262 && args[0] as i32 != -100 && !path.starts_with('/')
                                    {
                                        return Err(Error::UnsupportedSyscall(number));
                                    }
                                    let path = if path.is_empty() && flags & 4096 != 0 {
                                        "."
                                    } else {
                                        &path
                                    };
                                    Some(blink_host::path_stat(
                                        &config.rootfs,
                                        &process.cwd.borrow(),
                                        std::path::Path::new(path),
                                        flags & 256 != 0,
                                    ))
                                };
                                match stat {
                                    None => errno(9),
                                    Some(Err(e)) => blink_linux::errno(&e),
                                    Some(Ok(stat)) => {
                                        let bytes =
                                            blink_linux::stat_bytes(&stat, config.guest_arch);
                                        if process.memory.borrow_mut().write(buf, &bytes).is_err() {
                                            errno(14)
                                        } else {
                                            0
                                        }
                                    }
                                }
                            }
                        }
                    }
                    7 => {
                        if args[1] > 1024 {
                            errno(22)
                        } else if process
                            .memory
                            .borrow()
                            .check(args[0], args[1] as usize * 8, Access::Write)
                            .is_err()
                            || process
                                .memory
                                .borrow()
                                .check(args[0], args[1] as usize * 8, Access::Read)
                                .is_err()
                        {
                            errno(14)
                        } else {
                            let mut ready = 0;
                            for n in 0..args[1] {
                                let address = args[0] + n * 8;
                                let fd = process.memory.borrow().load(address, 4)? as i32;
                                let wanted = process.memory.borrow().load(address + 4, 2)? as u16;
                                let events = if fd < 0 {
                                    0
                                } else {
                                    poll_events(process.fds.borrow().get(&fd), wanted)
                                };
                                process
                                    .memory
                                    .borrow_mut()
                                    .store(address + 6, events as u64, 2)?;
                                ready += u64::from(events != 0);
                            }
                            let timeout = args[2] as i32;
                            if ready > 0
                                || timeout == 0
                                || process.poll_deadline.is_some_and(|d| Instant::now() >= d)
                            {
                                process.poll_deadline = None;
                                ready
                            } else {
                                if timeout > 0 && process.poll_deadline.is_none() {
                                    process.poll_deadline = Instant::now().checked_add(
                                        std::time::Duration::from_millis(timeout as u64),
                                    );
                                }
                                process.pending = Some((number, args));
                                event = Event::Blocked;
                                break;
                            }
                        }
                    }
                    40 => {
                        let fds = process.fds.borrow();
                        match (fds.get(&(args[0] as i32)), fds.get(&(args[1] as i32))) {
                            (None, _) | (_, None) => errno(9),
                            (
                                _,
                                Some(
                                    Fd::Reader(_)
                                    | Fd::Writer(_)
                                    | Fd::Stdin
                                    | Fd::Stdout
                                    | Fd::Stderr,
                                ),
                            ) => errno(22),
                            _ => return Err(Error::UnsupportedSyscall(number)),
                        }
                    }
                    16 => {
                        if process.fds.borrow().contains_key(&(args[0] as i32)) {
                            match args[1] {
                                // The embedded runtime exposes byte streams, not terminals.
                                0x5401 | 0x5402 | 0x5403 | 0x5404 | 0x5413 | 0x5414 => errno(25),
                                _ => return Err(Error::UnsupportedSyscall(number)),
                            }
                        } else {
                            errno(9)
                        }
                    }
                    72 => {
                        let fd = process.fds.borrow().get(&(args[0] as i32)).cloned();
                        match fd {
                            None => errno(9),
                            Some(fd) => match args[1] {
                                0 | 1030 => {
                                    if args[2] >= 1024 {
                                        errno(22)
                                    } else if let Some(dest) = (args[2] as i32..1024)
                                        .find(|n| !process.fds.borrow().contains_key(n))
                                    {
                                        process.fds.borrow_mut().insert(dest, fd.duplicate());
                                        if args[1] == 1030 {
                                            process.descriptor_flags.borrow_mut().insert(dest);
                                        } else {
                                            process.descriptor_flags.borrow_mut().remove(&dest);
                                        }
                                        dest as u64
                                    } else {
                                        errno(24)
                                    }
                                }
                                1 => {
                                    (fd.close_on_exec()
                                        || process
                                            .descriptor_flags
                                            .borrow()
                                            .contains(&(args[0] as i32)))
                                        as u64
                                }
                                2 => {
                                    let yes = args[2] & 1 != 0;
                                    if yes {
                                        process
                                            .descriptor_flags
                                            .borrow_mut()
                                            .insert(args[0] as i32);
                                    } else {
                                        process
                                            .descriptor_flags
                                            .borrow_mut()
                                            .remove(&(args[0] as i32));
                                    }
                                    if let Some(Fd::File { cloexec, .. }) =
                                        process.fds.borrow_mut().get_mut(&(args[0] as i32))
                                    {
                                        *cloexec = yes;
                                    }
                                    0
                                }
                                3 => match &fd {
                                    Fd::Stdin | Fd::Reader(_) => 0,
                                    Fd::Stdout | Fd::Stderr | Fd::Writer(_) => 1,
                                    Fd::File {
                                        read,
                                        write,
                                        append,
                                        ..
                                    } => {
                                        (if *read && *write {
                                            2
                                        } else if *write {
                                            1
                                        } else {
                                            0
                                        }) | (if *append { 1024 } else { 0 })
                                    }
                                },
                                _ => return Err(Error::UnsupportedSyscall(number)),
                            },
                        }
                    }
                    20 => {
                        if args[2] > 1024 {
                            errno(22)
                        } else {
                            let fd = process.fds.borrow().get(&(args[0] as i32)).cloned();
                            match fd.as_ref() {
                                None => errno(9),
                                Some(Fd::Writer(pipe)) => {
                                    let result = pipe_writev(
                                        &process.memory.borrow(),
                                        pipe,
                                        args[1],
                                        args[2] as usize,
                                    )?;
                                    match result {
                                        None => {
                                            process.pending = Some((number, args));
                                            event = Event::Blocked;
                                            break;
                                        }
                                        Some(result) => {
                                            if result == errno(32) {
                                                process.pending_signals |= 1 << 12;
                                                process.signal_senders[12] = process.group;
                                            }
                                            result
                                        }
                                    }
                                }
                                Some(Fd::Stdout | Fd::Stderr | Fd::File { write: true, .. }) => {
                                    if process
                                        .memory
                                        .borrow()
                                        .check(args[1], args[2] as usize * 16, Access::Read)
                                        .is_err()
                                    {
                                        errno(14)
                                    } else {
                                        let mut result = 0u64;
                                        'vectors: for n in 0..args[2] {
                                            let (address, length) = {
                                                let m = process.memory.borrow();
                                                (
                                                    m.load(args[1] + n * 16, 8)?,
                                                    m.load(args[1] + n * 16 + 8, 8)?,
                                                )
                                            };
                                            if length > 0x7ffff000 || result + length > 0x7ffff000 {
                                                if result == 0 {
                                                    result = errno(22);
                                                }
                                                break;
                                            }
                                            if process
                                                .memory
                                                .borrow()
                                                .check(address, length as usize, Access::Read)
                                                .is_err()
                                            {
                                                if result == 0 {
                                                    result = errno(14);
                                                }
                                                break;
                                            }
                                            let mut done = 0;
                                            let mut bytes = [0; 4096];
                                            while done < length {
                                                let count = (length - done).min(4096) as usize;
                                                process.memory.borrow().read(
                                                    address + done,
                                                    &mut bytes[..count],
                                                    Access::Read,
                                                )?;
                                                let wrote = match fd.as_ref().unwrap() {
                                                    Fd::Stdout => stdout
                                                        .write_all(&bytes[..count])
                                                        .map(|_| count),
                                                    Fd::Stderr => stderr
                                                        .write_all(&bytes[..count])
                                                        .map(|_| count),
                                                    Fd::File { file, .. } => {
                                                        file.borrow_mut().write(&bytes[..count])
                                                    }
                                                    _ => unreachable!(),
                                                };
                                                match wrote {
                                                    Ok(n) => {
                                                        result += n as u64;
                                                        done += n as u64;
                                                        if n < count {
                                                            break 'vectors;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        if matches!(
                                                            fd,
                                                            Some(Fd::Stdout | Fd::Stderr)
                                                        ) {
                                                            return Err(e.into());
                                                        }
                                                        if result == 0 {
                                                            result = blink_linux::errno(&e);
                                                        }
                                                        break 'vectors;
                                                    }
                                                }
                                            }
                                        }
                                        result
                                    }
                                }
                                Some(Fd::File { .. }) => errno(9),
                                _ => return Err(Error::UnsupportedSyscall(number)),
                            }
                        }
                    }
                    17 => {
                        let fd = process.fds.borrow().get(&(args[0] as i32)).cloned();
                        let size = args[2].min(65536) as usize;
                        match fd.as_ref() {
                            None => errno(9),
                            Some(Fd::File {
                                file, read: true, ..
                            }) => {
                                if process
                                    .memory
                                    .borrow()
                                    .check(args[1], size, Access::Write)
                                    .is_err()
                                {
                                    errno(14)
                                } else {
                                    let mut bytes = vec![0; size];
                                    match blink_host::read_at(&file.borrow(), &mut bytes, args[3]) {
                                        Err(e) => blink_linux::errno(&e),
                                        Ok(n) => {
                                            process
                                                .memory
                                                .borrow_mut()
                                                .write(args[1], &bytes[..n])?;
                                            n as u64
                                        }
                                    }
                                }
                            }
                            Some(Fd::File { .. }) => errno(9),
                            _ => errno(29),
                        }
                    }
                    5 => {
                        let fd = process.fds.borrow().get(&(args[0] as i32)).cloned();
                        match fd {
                            None => errno(9),
                            Some(fd) => match fd.stat() {
                                Err(e) => blink_linux::errno(&e),
                                Ok(stat) => {
                                    let bytes = blink_linux::stat_bytes(&stat, config.guest_arch);
                                    if process.memory.borrow_mut().write(args[1], &bytes).is_err() {
                                        errno(14)
                                    } else {
                                        0
                                    }
                                }
                            },
                        }
                    }
                    89 | 267 => {
                        let (ptr, buf, size) = if nr == 89 {
                            (args[0], args[1], args[2])
                        } else {
                            (args[1], args[2], args[3])
                        };
                        let path = cstring(&process.memory.borrow(), ptr);
                        match path {
                            Err(_) => errno(14),
                            Ok(path) => {
                                if nr == 267 && args[0] as i32 != -100 && !path.starts_with('/') {
                                    return Err(Error::UnsupportedSyscall(number));
                                }
                                let target = if path == "/proc/self/exe" {
                                    Ok(process.executable.clone())
                                } else {
                                    blink_host::read_link(
                                        &config.rootfs,
                                        &process.cwd.borrow(),
                                        std::path::Path::new(&path),
                                    )
                                };
                                match target {
                                    Err(e) => blink_linux::errno(&e),
                                    Ok(target) => {
                                        let bytes = target.as_os_str().as_encoded_bytes();
                                        let count = bytes.len().min(size as usize);
                                        if size == 0 {
                                            errno(22)
                                        } else if process
                                            .memory
                                            .borrow_mut()
                                            .write(buf, &bytes[..count])
                                            .is_err()
                                        {
                                            errno(14)
                                        } else {
                                            count as u64
                                        }
                                    }
                                }
                            }
                        }
                    }
                    302 => {
                        if args[2] != 0 {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        if args[0] != 0 && args[0] != process.group as u64 {
                            errno(3)
                        } else if args[1] > 15 {
                            errno(22)
                        } else if args[3] == 0 {
                            0
                        } else {
                            let limit = match args[1] {
                                2 | 9 => config.max_memory as u64,
                                3 => 1024 * 1024,
                                4 | 8 | 12 | 13 | 14 => 0,
                                6 | 7 | 11 => 1024,
                                _ => u64::MAX,
                            };
                            if process
                                .memory
                                .borrow()
                                .check(args[3], 16, Access::Write)
                                .is_err()
                            {
                                errno(14)
                            } else {
                                let mut m = process.memory.borrow_mut();
                                m.store(args[3], limit, 8)?;
                                m.store(args[3] + 8, limit, 8)?;
                                0
                            }
                        }
                    }
                    273 => {
                        if args[1] != 24 {
                            errno(22)
                        } else {
                            process.robust_head = args[0];
                            0
                        }
                    }
                    334 => errno(38), // rseq is explicitly unavailable; libc may use its documented fallback.
                    318 => {
                        if args[2] & !3 != 0 {
                            errno(22)
                        } else {
                            let count = args[1].min(65536) as usize;
                            if process
                                .memory
                                .borrow()
                                .check(args[0], count, Access::Write)
                                .is_err()
                            {
                                errno(14)
                            } else {
                                let mut bytes = vec![0; count];
                                blink_host::fill_random(&mut bytes)?;
                                process.memory.borrow_mut().write(args[0], &bytes)?;
                                count as u64
                            }
                        }
                    }
                    12 => {
                        let mut heap = process.heap.borrow_mut();
                        let requested = args[0];
                        if requested >= heap.base && requested < 0x800000000000 {
                            let old = (heap.brk + 4095) & !4095;
                            let new = (requested + 4095) & !4095;
                            let total =
                                memory_usage(processes.values().chain(std::iter::once(&process)));
                            let changed = if new > old {
                                if total
                                    .checked_add((new - old) as usize)
                                    .is_none_or(|n| n > config.max_memory)
                                {
                                    false
                                } else {
                                    process
                                        .memory
                                        .borrow_mut()
                                        .map(old, new - old, Permissions::RW)
                                        .is_ok()
                                }
                            } else if new < old {
                                process.memory.borrow_mut().unmap(new, old - new).is_ok()
                            } else {
                                true
                            };
                            if changed {
                                heap.brk = requested;
                            }
                        }
                        heap.brk
                    }
                    9 => map_memory(config, &process, &processes, args)?,
                    10 | 11 => {
                        if args[0] & 4095 != 0
                            || (nr == 11 && args[1] == 0)
                            || (nr == 10 && args[2] & !7 != 0)
                        {
                            errno(22)
                        } else {
                            let result = if nr == 11 {
                                process.memory.borrow_mut().unmap(args[0], args[1])
                            } else {
                                process.memory.borrow_mut().protect(
                                    args[0],
                                    args[1],
                                    Permissions {
                                        read: args[2] & 1 != 0,
                                        write: args[2] & 2 != 0,
                                        execute: args[2] & 4 != 0,
                                    },
                                )
                            };
                            if result.is_ok() {
                                0
                            } else {
                                errno(12)
                            }
                        }
                    }
                    13 => {
                        let signal = args[0];
                        if signal == 0 || signal > 64 || signal == 9 || signal == 19 || args[3] != 8
                        {
                            errno(22)
                        } else {
                            let old = process.actions.borrow()[(signal - 1) as usize];
                            let new = if args[1] != 0 {
                                let m = process.memory.borrow();
                                if m.check(args[1], 32, Access::Read).is_err() {
                                    None
                                } else {
                                    Some(SignalAction {
                                        handler: m.load(args[1], 8)?,
                                        flags: m.load(args[1] + 8, 8)?,
                                        restorer: m.load(args[1] + 16, 8)?,
                                        mask: m.load(args[1] + 24, 8)?,
                                    })
                                }
                            } else {
                                Some(old)
                            };
                            match new.filter(|_| {
                                args[2] == 0
                                    || process
                                        .memory
                                        .borrow()
                                        .check(args[2], 32, Access::Write)
                                        .is_ok()
                            }) {
                                None => errno(14),
                                Some(new) => {
                                    if new.flags & !(4 | 0x4000000 | 0x40000000 | 0x80000000) != 0 {
                                        return Err(Error::UnsupportedSyscall(number));
                                    }
                                    if new.handler > 1
                                        && (new.flags & 0x4000000 == 0 || new.restorer == 0)
                                    {
                                        return Err(Error::UnsupportedSyscall(number));
                                    }
                                    if args[2] != 0 {
                                        let mut m = process.memory.borrow_mut();
                                        for (n, v) in
                                            [old.handler, old.flags, old.restorer, old.mask]
                                                .iter()
                                                .enumerate()
                                        {
                                            m.store(args[2] + n as u64 * 8, *v, 8)?;
                                        }
                                    }
                                    if args[1] != 0 {
                                        process.actions.borrow_mut()[(signal - 1) as usize] = new;
                                    }
                                    0
                                }
                            }
                        }
                    }
                    14 => {
                        if args[3] != 8 || (args[0] > 2 && args[1] != 0) {
                            errno(22)
                        } else {
                            let new = if args[1] != 0 {
                                process.memory.borrow().load(args[1], 8)
                            } else {
                                Ok(0)
                            };
                            match new.ok().filter(|_| {
                                args[2] == 0
                                    || process
                                        .memory
                                        .borrow()
                                        .check(args[2], 8, Access::Write)
                                        .is_ok()
                            }) {
                                None => errno(14),
                                Some(value) => {
                                    if args[2] != 0 {
                                        process.memory.borrow_mut().store(
                                            args[2],
                                            process.signal_mask,
                                            8,
                                        )?;
                                    }
                                    if args[1] != 0 {
                                        process.signal_mask = match args[0] {
                                            0 => process.signal_mask | value,
                                            1 => process.signal_mask & !value,
                                            _ => value,
                                        } & !UNBLOCKABLE;
                                    }
                                    0
                                }
                            }
                        }
                    }
                    15 => {
                        let restored = match &mut process.cpu {
                            Engine::X86(c) => c.restore_signal(&process.memory.borrow()),
                            _ => return Err(Error::UnsupportedSyscall(number)),
                        };
                        match restored {
                            Ok(mask) => process.signal_mask = mask & !UNBLOCKABLE,
                            Err(_) => {
                                event = Event::Signal(11);
                                break;
                            }
                        }
                        continue;
                    }
                    2 | 257 => {
                        let (pathptr, flags, mode) = if nr == 2 {
                            (args[0], args[1], args[2])
                        } else {
                            (args[1], args[2], args[3])
                        };
                        let path = cstring(&process.memory.borrow(), pathptr);
                        match path {
                            Err(_) => errno(14),
                            Ok(path) => {
                                if nr == 257 && args[0] as i32 != -100 && !path.starts_with('/') {
                                    return Err(Error::UnsupportedSyscall(number));
                                }
                                if flags & !(3 | 64 | 128 | 512 | 1024 | 0x8000 | 0x80000) != 0 {
                                    return Err(Error::UnsupportedSyscall(number));
                                }
                                if flags & 3 == 3 || flags & 512 != 0 && flags & 3 == 0 {
                                    errno(22)
                                } else if let Some(fd) = process.fd() {
                                    match open_file(
                                        config,
                                        &process.cwd.borrow(),
                                        &path,
                                        flags,
                                        mode,
                                    ) {
                                        Ok(file) => {
                                            process.fds.borrow_mut().insert(fd, file);
                                            fd as u64
                                        }
                                        Err(e) => blink_linux::errno(&e),
                                    }
                                } else {
                                    errno(24)
                                }
                            }
                        }
                    }
                    8 => {
                        let fd = process.fds.borrow().get(&(args[0] as i32)).cloned();
                        match fd.as_ref() {
                            None => errno(9),
                            Some(Fd::File { file, .. }) => {
                                let seek = match args[2] {
                                    0 => {
                                        if args[1] as i64 >= 0 {
                                            Some(SeekFrom::Start(args[1]))
                                        } else {
                                            None
                                        }
                                    }
                                    1 => Some(SeekFrom::Current(args[1] as i64)),
                                    2 => Some(SeekFrom::End(args[1] as i64)),
                                    _ => None,
                                };
                                match seek {
                                    None => errno(22),
                                    Some(seek) => match file.borrow_mut().seek(seek) {
                                        Ok(v) => v,
                                        Err(e) => blink_linux::errno(&e),
                                    },
                                }
                            }
                            _ => errno(29),
                        }
                    }
                    79 => {
                        let cwd = process.cwd.borrow();
                        let bytes = cwd.as_os_str().as_encoded_bytes();
                        if args[1] < bytes.len() as u64 + 1 {
                            errno(34)
                        } else if process
                            .memory
                            .borrow()
                            .check(args[0], bytes.len() + 1, Access::Write)
                            .is_err()
                        {
                            errno(14)
                        } else {
                            process.memory.borrow_mut().write(args[0], bytes)?;
                            process.memory.borrow_mut().store(
                                args[0] + bytes.len() as u64,
                                0,
                                1,
                            )?;
                            bytes.len() as u64 + 1
                        }
                    }
                    80 => {
                        let path = cstring(&process.memory.borrow(), args[0]);
                        let cwd = process.cwd.borrow().clone();
                        match path {
                            Err(_) => errno(14),
                            Ok(path) => match blink_host::resolve_from(
                                &config.rootfs,
                                &cwd,
                                std::path::Path::new(&path),
                                false,
                            ) {
                                Err(e) => blink_linux::errno(&e),
                                Ok(p) => {
                                    if !p.is_dir() {
                                        errno(20)
                                    } else {
                                        let cwd = std::path::Path::new("/")
                                            .join(p.strip_prefix(&config.rootfs).unwrap());
                                        *process.cwd.borrow_mut() = cwd;
                                        0
                                    }
                                }
                            },
                        }
                    }
                    39 => process.group as u64,
                    186 => pid as u64,
                    158 => match args[0] {
                        0x1002 => {
                            if args[1] > 0x7fff_ffff_ffff {
                                errno(1)
                            } else {
                                process.cpu.tls(args[1]);
                                0
                            }
                        }
                        0x1003 => {
                            let base = match &process.cpu {
                                Engine::X86(c) => c.fs_base,
                                _ => unreachable!(),
                            };
                            if process.memory.borrow_mut().store(args[1], base, 8).is_err() {
                                errno(14)
                            } else {
                                0
                            }
                        }
                        _ => return Err(Error::UnsupportedSyscall(number)),
                    },
                    218 => {
                        process.clear_tid = args[0];
                        pid as u64
                    }
                    110 => process.parent as u64,
                    57 | 56 => {
                        let thread = nr == 56 && args[0] & 0x10000 != 0;
                        let flags = args[0];
                        if nr == 56
                            && ((!thread && (flags != 17 || args[1] != 0))
                                || (thread
                                    && (flags & 0x10f00 != 0x10f00
                                        || flags
                                            & !(0x10f00
                                                | 0x40000
                                                | 0x80000
                                                | 0x100000
                                                | 0x200000
                                                | 0x1000000)
                                            != 0
                                        || args[1] == 0)))
                        {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        let child_tid = if config.guest_arch == GuestArch::X86_64 {
                            args[3]
                        } else {
                            args[4]
                        };
                        let bad_tid = thread
                            && ((flags & 0x100000 != 0
                                && process
                                    .memory
                                    .borrow()
                                    .check(args[2], 4, Access::Write)
                                    .is_err())
                                || (flags & 0x1000000 != 0
                                    && process
                                        .memory
                                        .borrow()
                                        .check(child_tid, 4, Access::Write)
                                        .is_err()));
                        let total =
                            memory_usage(processes.values().chain(std::iter::once(&process)));
                        if bad_tid {
                            errno(14)
                        } else if processes.len() + zombies.len() >= 1023
                            || (!thread
                                && total
                                    .checked_add(process.memory.borrow().mapped_bytes())
                                    .is_none_or(|n| n > config.max_memory))
                        {
                            errno(11)
                        } else {
                            let child_pid = next_pid;
                            next_pid = next_pid.checked_add(1).ok_or(Error::ResourceLimit)?;
                            let mut child = process.clone();
                            child.clear_tid = 0;
                            child.robust_head = 0;
                            child.pending_signals = 0;
                            child.signal_senders = [0; 64];
                            child.futex = None;
                            child.pending = None;
                            if thread {
                                child.cpu.stack(args[1]);
                                if flags & 0x80000 != 0 {
                                    child.cpu.tls(if config.guest_arch == GuestArch::X86_64 {
                                        args[4]
                                    } else {
                                        args[3]
                                    });
                                }
                                if flags & 0x100000 != 0 {
                                    process.memory.borrow_mut().store(
                                        args[2],
                                        child_pid as u64,
                                        4,
                                    )?;
                                }
                                if flags & 0x1000000 != 0 {
                                    process.memory.borrow_mut().store(
                                        child_tid,
                                        child_pid as u64,
                                        4,
                                    )?;
                                }
                                if flags & 0x200000 != 0 {
                                    child.clear_tid = child_tid;
                                }
                            } else {
                                child.memory =
                                    Rc::new(RefCell::new(process.memory.borrow().clone()));
                                child.fds = Rc::new(RefCell::new(process.fds.borrow().clone()));
                                child.cwd = Rc::new(RefCell::new(process.cwd.borrow().clone()));
                                child.actions = Rc::new(RefCell::new(*process.actions.borrow()));
                                child.heap = Rc::new(RefCell::new(process.heap.borrow().clone()));
                                child.descriptor_flags = Rc::new(RefCell::new(
                                    process.descriptor_flags.borrow().clone(),
                                ));
                                child.group = child_pid;
                                child.parent = process.group;
                            }
                            child.cpu.cpu().set_result(0);
                            processes.insert(child_pid, child);
                            queue.push_back(child_pid);
                            child_pid as u64
                        }
                    }
                    202 => {
                        if args[0] & 3 != 0 {
                            errno(22)
                        } else if args[1] == 0 || args[1] == 128 {
                            if args[3] != 0 {
                                return Err(Error::UnsupportedSyscall(number));
                            }
                            let value = process.memory.borrow().load(args[0], 4);
                            match value {
                                Err(_) => errno(14),
                                Ok(v) if v != args[2] as u32 as u64 => errno(11),
                                Ok(_) => {
                                    process.futex = Some(args[0]);
                                    process.cpu.cpu().set_result(0);
                                    event = Event::Blocked;
                                    break;
                                }
                            }
                        } else if args[1] == 1 || args[1] == 129 {
                            if (args[2] as u32 as i32) < 0 {
                                errno(22)
                            } else {
                                wake(&mut processes, &process.memory, args[0], args[2] as u32)
                                    as u64
                            }
                        } else {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                    }
                    61 => {
                        let wanted = args[0] as i64;
                        if args[2] & !1 != 0 || args[3] != 0 || wanted == 0 || wanted < -1 {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        let matches = |id: u32, parent: u32| {
                            parent == process.group && (wanted == -1 || wanted == id as i64)
                        };
                        if let Some(child) = zombies
                            .iter()
                            .find(|(id, z)| matches(**id, z.parent))
                            .map(|(id, _)| *id)
                        {
                            if args[1] != 0
                                && process
                                    .memory
                                    .borrow_mut()
                                    .store(args[1], zombies[&child].status as u64, 4)
                                    .is_err()
                            {
                                errno(14)
                            } else {
                                zombies.remove(&child);
                                child as u64
                            }
                        } else if processes.values().any(|p| matches(p.group, p.parent)) {
                            if args[2] & 1 != 0 {
                                0
                            } else {
                                process.pending = Some((number, args));
                                event = Event::Blocked;
                                break;
                            }
                        } else {
                            errno(10)
                        }
                    }
                    59 => {
                        if processes.values().any(|p| p.group == process.group) {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        let values = (|| {
                            Ok::<_, Error>((
                                cstring(&process.memory.borrow(), args[0])?,
                                strings(&process.memory.borrow(), args[1])?,
                                strings(&process.memory.borrow(), args[2])?,
                            ))
                        })();
                        match values {
                            Err(_) => errno(14),
                            Ok((path, argv, env)) => {
                                match image(
                                    config,
                                    std::path::Path::new(&path),
                                    &process.cwd.borrow(),
                                    &argv,
                                    &env,
                                    &process.credentials,
                                ) {
                                    Ok(new) => {
                                        let other = memory_usage(processes.values());
                                        if other
                                            .checked_add(new.memory.mapped_bytes())
                                            .is_none_or(|n| n > config.max_memory)
                                        {
                                            errno(12)
                                        } else {
                                            process.cpu = Engine::new(
                                                config.guest_arch,
                                                new.entry,
                                                new.stack,
                                            );
                                            process.memory = Rc::new(RefCell::new(new.memory));
                                            process.executable = guest_executable(
                                                config,
                                                &process.cwd.borrow(),
                                                std::path::Path::new(&path),
                                            )?;
                                            process.robust_head = 0;
                                            process.clear_tid = 0;
                                            process.heap =
                                                Rc::new(RefCell::new(Heap::new(new.heap)));
                                            for action in process.actions.borrow_mut().iter_mut() {
                                                if action.handler > 1 {
                                                    *action = SignalAction::default();
                                                }
                                            }
                                            process.fds.borrow_mut().retain(|number, fd| {
                                                !fd.close_on_exec()
                                                    && !process
                                                        .descriptor_flags
                                                        .borrow()
                                                        .contains(number)
                                            });
                                            process.descriptor_flags.borrow_mut().clear();
                                            continue;
                                        }
                                    }
                                    Err(Error::InvalidElf(_)) => errno(8),
                                    Err(Error::ResourceLimit) => errno(12),
                                    Err(_) => errno(2),
                                }
                            }
                        }
                    }
                    22 | 293 => {
                        if nr == 293 && args[1] & !0x80000 != 0 {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        if process
                            .memory
                            .borrow()
                            .check(args[0], 8, Access::Write)
                            .is_err()
                        {
                            errno(14)
                        } else if process.fds.borrow().len() > 1022 {
                            errno(24)
                        } else {
                            let p = Rc::new(RefCell::new(Pipe {
                                inode: next_pipe,
                                bytes: VecDeque::new(),
                                readers: 1,
                                writers: 1,
                            }));
                            next_pipe += 1;
                            let read = process.fd().unwrap();
                            process.fds.borrow_mut().insert(read, Fd::Reader(p.clone()));
                            let write = process.fd().unwrap();
                            process.fds.borrow_mut().insert(write, Fd::Writer(p));
                            if nr == 293 && args[1] & 0x80000 != 0 {
                                process.descriptor_flags.borrow_mut().extend([read, write]);
                            }
                            process.memory.borrow_mut().store(args[0], read as u64, 4)?;
                            process
                                .memory
                                .borrow_mut()
                                .store(args[0] + 4, write as u64, 4)?;
                            0
                        }
                    }
                    3 => {
                        process
                            .descriptor_flags
                            .borrow_mut()
                            .remove(&(args[0] as i32));
                        if process.fds.borrow_mut().remove(&(args[0] as i32)).is_some() {
                            0
                        } else {
                            errno(9)
                        }
                    }
                    32 | 33 | 292 => {
                        if nr == 292 && args[2] & !0x80000 != 0 {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        let fd = process.fds.borrow().get(&(args[0] as i32)).map(|fd| {
                            if nr == 33 && args[0] == args[1] {
                                fd.clone()
                            } else {
                                fd.duplicate()
                            }
                        });
                        if let Some(fd) = fd {
                            let dest = if nr == 32 {
                                process.fd().unwrap_or(-1)
                            } else {
                                args[1] as i32
                            };
                            if nr == 292 && args[0] == args[1] {
                                errno(22)
                            } else if !(0..1024).contains(&dest) {
                                errno(if nr == 32 { 24 } else { 9 })
                            } else {
                                if nr != 33 || args[0] != args[1] {
                                    process.descriptor_flags.borrow_mut().remove(&dest);
                                }
                                if nr == 292 && args[2] & 0x80000 != 0 {
                                    process.descriptor_flags.borrow_mut().insert(dest);
                                }
                                process.fds.borrow_mut().insert(dest, fd);
                                dest as u64
                            }
                        } else {
                            errno(9)
                        }
                    }
                    0 | 1 => {
                        let fd = process.fds.borrow().get(&(args[0] as i32)).cloned();
                        if let Some(fd) = fd {
                            let count = args[2].min(0x7ffff000) as usize;
                            if process
                                .memory
                                .borrow()
                                .check(
                                    args[1],
                                    count,
                                    if nr == 0 { Access::Write } else { Access::Read },
                                )
                                .is_err()
                            {
                                errno(14)
                            } else if count == 0 {
                                0
                            } else {
                                match (nr, &fd) {
                                    (0, Fd::Stdin) => 0,
                                    (
                                        0,
                                        Fd::File {
                                            file, read: true, ..
                                        },
                                    ) => {
                                        let mut bytes = vec![0; count.min(65536)];
                                        match file.borrow_mut().read(&mut bytes) {
                                            Ok(n) => {
                                                process
                                                    .memory
                                                    .borrow_mut()
                                                    .write(args[1], &bytes[..n])?;
                                                n as u64
                                            }
                                            Err(e) => blink_linux::errno(&e),
                                        }
                                    }
                                    (
                                        1,
                                        Fd::File {
                                            file, write: true, ..
                                        },
                                    ) => {
                                        let mut bytes = vec![0; count.min(65536)];
                                        process.memory.borrow().read(
                                            args[1],
                                            &mut bytes,
                                            Access::Read,
                                        )?;
                                        match file.borrow_mut().write(&bytes) {
                                            Ok(n) => n as u64,
                                            Err(e) => blink_linux::errno(&e),
                                        }
                                    }
                                    (0, Fd::Reader(pipe)) => {
                                        let mut pipe = pipe.borrow_mut();
                                        if pipe.bytes.is_empty() && pipe.writers != 0 {
                                            process.pending = Some((number, args));
                                            event = Event::Blocked;
                                            break;
                                        }
                                        let count = count.min(pipe.bytes.len());
                                        let bytes: Vec<_> = pipe.bytes.drain(..count).collect();
                                        process.memory.borrow_mut().write(args[1], &bytes)?;
                                        count as u64
                                    }
                                    (1, Fd::Writer(pipe)) => {
                                        let mut pipe = pipe.borrow_mut();
                                        if pipe.readers == 0 {
                                            process.pending_signals |= 1 << 12;
                                            process.signal_senders[12] = process.group;
                                            process.cpu.cpu().set_result(errno(32));
                                            continue;
                                        }
                                        let capacity = 65536 - pipe.bytes.len();
                                        if capacity == 0 || (count <= 4096 && capacity < count) {
                                            process.pending = Some((number, args));
                                            event = Event::Blocked;
                                            break;
                                        }
                                        let count = count.min(capacity);
                                        let mut bytes = vec![0; count];
                                        process.memory.borrow().read(
                                            args[1],
                                            &mut bytes,
                                            Access::Read,
                                        )?;
                                        pipe.bytes.extend(bytes);
                                        count as u64
                                    }
                                    (1, Fd::Stdout) | (1, Fd::Stderr) => {
                                        let sink: &mut dyn Write = if matches!(fd, Fd::Stdout) {
                                            &mut *stdout
                                        } else {
                                            &mut *stderr
                                        };
                                        let mut buf = [0; 4096];
                                        let mut done = 0;
                                        while done < count {
                                            let n = (count - done).min(buf.len());
                                            process.memory.borrow().read(
                                                args[1] + done as u64,
                                                &mut buf[..n],
                                                Access::Read,
                                            )?;
                                            sink.write_all(&buf[..n])?;
                                            done += n;
                                        }
                                        done as u64
                                    }
                                    _ => errno(9),
                                }
                            }
                        } else {
                            errno(9)
                        }
                    }
                    62 => {
                        let target = args[0] as u32;
                        let signal = args[1];
                        if args[0] > u32::MAX as u64 || target == 0 || signal > 64 {
                            errno(22)
                        } else if target != process.group
                            && !processes.values().any(|p| p.group == target)
                            && !zombies.contains_key(&target)
                        {
                            errno(3)
                        } else if signal == 0 {
                            0
                        } else if signal == 19 {
                            return Err(Error::UnsupportedSyscall(number));
                        } else {
                            let bit = 1u64 << (signal - 1);
                            if target == process.group
                                && (process.signal_mask & bit == 0
                                    || !processes
                                        .values()
                                        .any(|p| p.group == target && p.signal_mask & bit == 0))
                            {
                                process.pending_signals |= bit;
                                process.signal_senders[(signal - 1) as usize] = process.group;
                            } else {
                                let chosen = processes
                                    .iter()
                                    .find(|(_, p)| p.group == target && p.signal_mask & bit == 0)
                                    .or_else(|| processes.iter().find(|(_, p)| p.group == target))
                                    .map(|(id, _)| *id);
                                if let Some(id) = chosen {
                                    let p = processes.get_mut(&id).unwrap();
                                    p.pending_signals |= bit;
                                    p.signal_senders[(signal - 1) as usize] = process.group;
                                }
                            }
                            0
                        }
                    }
                    _ => return Err(Error::UnsupportedSyscall(number)),
                };
                process.cpu.cpu().set_result(ret);
            }
        }
        match event {
            Event::Exit(code) | Event::Signal(code) => {
                let signaled = matches!(event, Event::Signal(_));
                release_robust(&process, pid, &mut processes);
                clear_tid(&process, &mut processes);
                if signaled {
                    processes.retain(|_, p| p.group != process.group);
                    group_status.remove(&process.group);
                }
                if processes.values().any(|p| p.group == process.group) {
                    if pid == process.group {
                        group_status.insert(process.group, code);
                    }
                    continue 'scheduler;
                }
                let code = group_status.remove(&process.group).unwrap_or(code);
                if process.group == 1 {
                    break 'scheduler if signaled {
                        Termination::Signaled(code)
                    } else {
                        Termination::Exited(code)
                    };
                }
                zombies.insert(
                    process.group,
                    Zombie {
                        parent: process.parent,
                        status: if signaled { code } else { code << 8 },
                    },
                );
                for p in processes.values_mut() {
                    if p.parent == process.group {
                        p.parent = 1;
                    }
                }
                for z in zombies.values_mut() {
                    if z.parent == process.group {
                        z.parent = 1;
                    }
                }
            }
            Event::Ready | Event::Blocked => {
                processes.insert(pid, process);
                queue.push_back(pid);
            }
        }
        if !processes.is_empty()
            && processes
                .values()
                .all(|p| p.pending.is_some() || p.futex.is_some())
        {
            // Poll cancellation without spinning when all guests wait on each other.
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    // Dropping this owned process table closes every descriptor and destroys all descendants.
    Ok(RunOutcome {
        termination,
        instructions,
    })
}

fn memory_usage<'a>(processes: impl Iterator<Item = &'a Process>) -> usize {
    let mut spaces = BTreeMap::new();
    for p in processes {
        spaces
            .entry(Rc::as_ptr(&p.memory))
            .or_insert_with(|| p.memory.borrow().mapped_bytes());
    }
    spaces.values().sum()
}
fn wake(
    processes: &mut BTreeMap<u32, Process>,
    memory: &Rc<RefCell<Memory>>,
    address: u64,
    count: u32,
) -> u32 {
    let mut n = 0;
    for p in processes.values_mut() {
        if n >= count {
            break;
        }
        if Rc::ptr_eq(&p.memory, memory) && p.futex == Some(address) {
            p.futex = None;
            n += 1;
        }
    }
    n
}
fn clear_tid(process: &Process, processes: &mut BTreeMap<u32, Process>) {
    if process.clear_tid != 0
        && process
            .memory
            .borrow_mut()
            .store(process.clear_tid, 0, 4)
            .is_ok()
    {
        wake(processes, &process.memory, process.clear_tid, u32::MAX);
    }
}

fn open_file(
    config: &RuntimeConfig,
    cwd: &std::path::Path,
    path: &str,
    flags: u64,
    mode: u64,
) -> std::io::Result<Fd> {
    let path = blink_host::resolve_from(
        &config.rootfs,
        cwd,
        std::path::Path::new(path),
        flags & 64 != 0,
    )?;
    if path.exists() && !path.is_file() {
        return Err(std::io::Error::new(
            if path.is_dir() {
                std::io::ErrorKind::IsADirectory
            } else {
                std::io::ErrorKind::InvalidInput
            },
            "only regular files are supported",
        ));
    }
    let read = flags & 3 != 1;
    let write = flags & 3 != 0;
    let mut options = std::fs::OpenOptions::new();
    options
        .read(read)
        .write(write)
        .create(flags & 64 != 0)
        .create_new(flags & (64 | 128) == (64 | 128))
        .truncate(flags & 512 != 0)
        .append(flags & 1024 != 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode((mode & 0o777) as u32);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let file = options.open(path)?;
    Ok(Fd::File {
        file: Rc::new(RefCell::new(file)),
        read,
        write,
        cloexec: flags & 0x80000 != 0,
        append: flags & 1024 != 0,
    })
}

fn deliver_signal(process: &mut Process) -> Result<Option<i32>> {
    loop {
        let pending = process.pending_signals & !process.signal_mask;
        if pending == 0 {
            return Ok(None);
        }
        let n = pending.trailing_zeros() as usize;
        process.pending_signals &= !(1u64 << n);
        let signal = n + 1;
        let action = process.actions.borrow()[n];
        if action.handler == 1 || (action.handler == 0 && matches!(signal, 17 | 18 | 23 | 28)) {
            continue;
        }
        if action.handler == 0 || signal == 9 {
            return Ok(Some(signal as i32));
        }
        let blocked = process.pending.take().is_some();
        process.poll_deadline = None;
        let futex = process.futex.take().is_some();
        if blocked || futex {
            process.cpu.cpu().set_result(errno(4));
        }
        match &mut process.cpu {
            Engine::X86(cpu) => {
                if cpu
                    .enter_signal(
                        &mut process.memory.borrow_mut(),
                        signal as u32,
                        action.handler,
                        action.restorer,
                        process.signal_mask,
                        process.signal_senders[n],
                    )
                    .is_err()
                {
                    return Ok(Some(11));
                }
            }
            _ => return Err(Error::UnsupportedSyscall(134)),
        }
        process.signal_mask |= action.mask;
        if action.flags & 0x40000000 == 0 {
            process.signal_mask |= 1u64 << n;
        }
        process.signal_mask &= !UNBLOCKABLE;
        if action.flags & 0x80000000 != 0 {
            process.actions.borrow_mut()[n] = SignalAction::default();
        }
        return Ok(None);
    }
}

fn release_robust(process: &Process, tid: u32, processes: &mut BTreeMap<u32, Process>) {
    let head = process.robust_head;
    if head == 0 {
        return;
    }
    let header = (|| {
        let m = process.memory.borrow();
        Ok::<_, Error>((
            m.load(head, 8)?,
            m.load(head.wrapping_add(8), 8)?,
            m.load(head.wrapping_add(16), 8)?,
        ))
    })();
    let Ok((mut node, offset, pending)) = header else {
        return;
    };
    let mut release = |node: u64| {
        if node & 1 != 0 {
            return;
        }
        let address = node.wrapping_add(offset);
        let old = process.memory.borrow().load(address, 4);
        if let Ok(value) = old {
            if value & 0x3fffffff == tid as u64 {
                let new = (value & 0x80000000) | 0x40000000;
                if process.memory.borrow_mut().store(address, new, 4).is_ok() {
                    wake(processes, &process.memory, address, 1);
                }
            }
        }
    };
    for _ in 0..2048 {
        if node & !1 == head || node == 0 {
            break;
        }
        let next = process.memory.borrow().load(node & !1, 8);
        if node != pending {
            release(node);
        }
        match next {
            Ok(next) => node = next,
            Err(_) => break,
        }
    }
    if pending != 0 {
        release(pending);
    }
}

fn guest_executable(
    config: &RuntimeConfig,
    cwd: &std::path::Path,
    path: &std::path::Path,
) -> Result<std::path::PathBuf> {
    let path = blink_host::resolve_from(&config.rootfs, cwd, path, false)?;
    Ok(std::path::Path::new("/").join(
        path.strip_prefix(&config.rootfs)
            .map_err(|_| Error::InvalidConfig("executable root".into()))?,
    ))
}

fn map_memory(
    config: &RuntimeConfig,
    process: &Process,
    processes: &BTreeMap<u32, Process>,
    args: [u64; 6],
) -> Result<u64> {
    let flags = args[3];
    let anonymous = flags & 32 != 0;
    let fixed = flags & 16 != 0;
    let exclusive = flags & 0x100000 != 0;
    if flags & !(2 | 16 | 32 | 0x800 | 0x1000 | 0x4000 | 0x8000 | 0x20000 | 0x100000) != 0
        || flags & 2 == 0
    {
        return Err(Error::UnsupportedSyscall(9));
    }
    if args[1] == 0
        || args[2] & !7 != 0
        || args[5] & 4095 != 0
        || ((fixed || exclusive) && args[0] & 4095 != 0)
    {
        return Ok(errno(22));
    }
    if args[1] > config.max_memory as u64 {
        return Ok(errno(12));
    }
    let size = (args[1] + 4095) & !4095;
    let address = if fixed || exclusive {
        args[0]
    } else {
        process.heap.borrow().next_map
    };
    if address
        .checked_add(size)
        .is_none_or(|end| end >= 0x800000000000)
    {
        return Ok(errno(12));
    }
    let replaced = process.memory.borrow().mapped_in_range(address, size);
    if replaced != 0 && (!fixed || exclusive) {
        return Ok(errno(if exclusive { 17 } else { 12 }));
    }
    let total = memory_usage(processes.values().chain(std::iter::once(process)));
    if total
        .checked_sub(replaced)
        .and_then(|n| n.checked_add(size as usize))
        .is_none_or(|n| n > config.max_memory)
    {
        return Ok(errno(12));
    }
    let mut data = Vec::new();
    if !anonymous {
        let fd = process.fds.borrow().get(&(args[4] as i32)).cloned();
        match fd.as_ref() {
            Some(Fd::File {
                file, read: true, ..
            }) => {
                let file = file.borrow();
                let length = match file.metadata() {
                    Ok(m) => m.len(),
                    Err(e) => return Ok(blink_linux::errno(&e)),
                };
                data.resize(length.saturating_sub(args[5]).min(size) as usize, 0);
                let mut done = 0;
                while done < data.len() {
                    match blink_host::read_at(&file, &mut data[done..], args[5] + done as u64) {
                        Ok(0) => {
                            data.truncate(done);
                            break;
                        }
                        Ok(n) => done += n,
                        Err(e) => return Ok(blink_linux::errno(&e)),
                    }
                }
            }
            Some(Fd::File { .. }) => return Ok(errno(13)),
            Some(_) => return Ok(errno(19)),
            None => return Ok(errno(9)),
        }
    }
    let mut memory = process.memory.borrow_mut();
    if fixed {
        memory.unmap(address, size)?;
    }
    let permissions = Permissions {
        read: args[2] & 1 != 0,
        write: args[2] & 2 != 0,
        execute: args[2] & 4 != 0,
    };
    memory.map(address, size, permissions)?;
    if !anonymous {
        memory.initialize(address, &data)?;
        let backed = (data.len() as u64 + 4095) & !4095;
        if backed < size {
            memory.mark_bus_fault(address + backed, size - backed);
        }
    }
    if !fixed && !exclusive {
        process.heap.borrow_mut().next_map = address + size;
    }
    Ok(address)
}

// Gather only the bytes that fit in a bounded virtual pipe. Writes no larger
// than Linux PIPE_BUF are atomic across the cooperative guest scheduler.
fn pipe_writev(
    memory: &Memory,
    pipe: &Rc<RefCell<Pipe>>,
    address: u64,
    count: usize,
) -> Result<Option<u64>> {
    if memory.check(address, count * 16, Access::Read).is_err() {
        return Ok(Some(errno(14)));
    }
    let mut vectors = Vec::with_capacity(count);
    let mut total = 0u64;
    for n in 0..count {
        let base = memory.load(address + n as u64 * 16, 8)?;
        let length = memory.load(address + n as u64 * 16 + 8, 8)?;
        if length > 0x7ffff000 || total + length > 0x7ffff000 {
            return Ok(Some(errno(22)));
        }
        total += length;
        vectors.push((base, length));
    }
    if total == 0 {
        return Ok(Some(0));
    }
    let mut pipe = pipe.borrow_mut();
    if pipe.readers == 0 {
        return Ok(Some(errno(32)));
    }
    let capacity = 65536 - pipe.bytes.len();
    if capacity == 0 || (total <= 4096 && (capacity as u64) < total) {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(capacity.min(total as usize));
    for (base, length) in vectors {
        let length = (length as usize).min(capacity - bytes.len());
        if length == 0 {
            continue;
        }
        if memory.check(base, length, Access::Read).is_err() {
            if bytes.is_empty() {
                return Ok(Some(errno(14)));
            }
            break;
        }
        let start = bytes.len();
        bytes.resize(start + length, 0);
        memory.read(base, &mut bytes[start..], Access::Read)?;
        if bytes.len() == capacity {
            break;
        }
    }
    let length = bytes.len();
    pipe.bytes.extend(bytes);
    Ok(Some(length as u64))
}

fn poll_events(fd: Option<&Fd>, wanted: u16) -> u16 {
    let available = match fd {
        None => return 32, // POLLNVAL
        Some(Fd::Reader(p)) => {
            let p = p.borrow();
            (if p.bytes.is_empty() { 0 } else { 1 | 64 }) | if p.writers == 0 { 16 } else { 0 }
        }
        Some(Fd::Writer(p)) => {
            let p = p.borrow();
            (if p.bytes.len() < 65536 { 4 | 256 } else { 0 }) | if p.readers == 0 { 8 } else { 0 }
        }
        Some(Fd::Stdin) => 1 | 64,
        Some(Fd::Stdout | Fd::Stderr) => 4 | 256,
        // Linux regular files are always ready, even if the following I/O
        // will fail because the descriptor's access mode does not permit it.
        Some(Fd::File { .. }) => 1 | 4 | 64 | 256,
    };
    available & (wanted | 8 | 16 | 32)
}

#[cfg(test)]
mod descriptor_tests {
    use super::*;
    #[test]
    fn atomic_pipe_vectors_wait_without_partial_mutation() {
        let mut memory = Memory::new(4096);
        memory.map(4096, 4096, Permissions::RW).unwrap();
        memory.write(4200, b"abcdef").unwrap();
        for (address, value) in [(4096, 4200), (4104, 3), (4112, 4203), (4120, 3)] {
            memory.store(address, value, 8).unwrap();
        }
        let pipe = Rc::new(RefCell::new(Pipe {
            readers: 1,
            writers: 1,
            ..Pipe::default()
        }));
        pipe.borrow_mut().bytes.resize(65533, 0);
        assert_eq!(pipe_writev(&memory, &pipe, 4096, 2).unwrap(), None);
        assert_eq!(pipe.borrow().bytes.len(), 65533);
        pipe.borrow_mut().bytes.clear();
        assert_eq!(pipe_writev(&memory, &pipe, 4096, 2).unwrap(), Some(6));
        assert_eq!(
            pipe.borrow().bytes.iter().copied().collect::<Vec<_>>(),
            b"abcdef"
        );
        pipe.borrow_mut().readers = 0;
        assert_eq!(
            pipe_writev(&memory, &pipe, 4096, 2).unwrap(),
            Some(errno(32))
        );
    }
    #[test]
    fn pipe_poll_reports_eof_and_broken_reader_independent_of_requested_bits() {
        let pipe = Rc::new(RefCell::new(Pipe {
            readers: 1,
            writers: 1,
            ..Pipe::default()
        }));
        let read = Fd::Reader(pipe.clone());
        let write = Fd::Writer(pipe.clone());
        assert_eq!(poll_events(Some(&read), 1), 0);
        assert_eq!(poll_events(Some(&write), 4), 4);
        pipe.borrow_mut().bytes.push_back(b'x');
        drop(write);
        assert_eq!(poll_events(Some(&read), 1), 17);
        assert_eq!(poll_events(Some(&read), 0), 16);
        assert_eq!(poll_events(None, 0), 32);
    }
}
