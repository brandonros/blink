//! Cooperative guest processes. No host fork, host signals, or blocking guest pipes.
mod credentials;
mod internet;
mod output;
mod poll;
mod signals;
mod socket;
mod time;
mod virtual_fs;
use crate::{elf, Command, RunOutcome, RuntimeConfig, Termination};
use blink_core::{Access, Cpu, Error, GuestArch, Memory, Permissions, Result, Step};
use blink_linux::{Fd, Pipe, SignalAction, VirtualContent, UNBLOCKABLE};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{Read, Seek, SeekFrom, Write},
    rc::Rc,
    time::Instant,
};
#[derive(Clone)]
enum Engine {
    X86(Box<blink_x86_64::X86>),
    Arm(Box<blink_aarch64::Aarch64>),
}
impl Engine {
    fn new(arch: GuestArch, entry: u64, stack: u64) -> Self {
        match arch {
            GuestArch::X86_64 => Self::X86(Box::new(blink_x86_64::X86::new(entry, stack))),
            GuestArch::Aarch64 => Self::Arm(Box::new(blink_aarch64::Aarch64::new(entry, stack))),
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
            Self::X86(c) => c.as_mut(),
            Self::Arm(c) => c.as_mut(),
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
    tid: u32,
    credentials: blink_linux::Credentials,
    executable: std::path::PathBuf,
    comm: [u8; 16],
    argv: Vec<String>,
    env: Vec<String>,
    cpu: Engine,
    memory: Rc<RefCell<Memory>>,
    heap: Rc<RefCell<Heap>>,
    fds: Rc<RefCell<BTreeMap<i32, Fd>>>,
    descriptor_flags: Rc<RefCell<BTreeSet<i32>>>,
    group: u32,
    cwd: Rc<RefCell<std::path::PathBuf>>,
    umask: Rc<Cell<u32>>,
    clear_tid: u64,
    robust_head: u64,
    actions: Rc<RefCell<[SignalAction; 64]>>,
    signal_mask: u64,
    suspend_mask: Option<u64>,
    signal_stack: blink_x86_64::SignalStack,
    signals: signals::Pending,
    futex: Option<u64>,
    futex_syscall: Option<u64>,
    poll_deadline: Option<Instant>,
    sleep_deadline: Option<Instant>,
    real_timer: Rc<RefCell<time::RealTimer>>,
    vfork_wait: Option<u32>,
    vfork_parent: Option<u32>,
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
    let mut path = path.to_path_buf();
    let mut args = args.to_vec();
    for depth in 0..=4 {
        let bytes = read_image(config, cwd, &path)?;
        if bytes.starts_with(b"#!") {
            if depth == 4 {
                return Err(Error::InvalidElf("script interpreter recursion".into()));
            }
            let line = bytes.get(2..bytes.len().min(256)).unwrap_or_default();
            let end = line.iter().position(|b| *b == b'\n');
            if end.is_none() && bytes.len() >= 256 {
                return Err(Error::InvalidElf("overlong interpreter line".into()));
            }
            let line = std::str::from_utf8(&line[..end.unwrap_or(line.len())])
                .map_err(|_| Error::InvalidElf("non-UTF8 interpreter".into()))?;
            let line = line.trim_matches([' ', '\t']);
            let (interpreter, option) = match line.find([' ', '\t']) {
                Some(n) => (&line[..n], line[n..].trim_matches([' ', '\t'])),
                None => (line, ""),
            };
            if interpreter.is_empty() {
                return Err(Error::InvalidElf("empty script interpreter".into()));
            }
            let mut script_args = vec![interpreter.to_owned()];
            if !option.is_empty() {
                script_args.push(option.to_owned());
            }
            script_args.push(path.to_string_lossy().into_owned());
            script_args.extend(args.into_iter().skip(1));
            args = script_args;
            path = interpreter.into();
            continue;
        }
        let interp = elf::interpreter(&bytes)?
            .map(|path| read_image(config, cwd, std::path::Path::new(&path)))
            .transpose()?;
        let mut loaded = elf::load(
            &bytes,
            interp.as_deref(),
            config.guest_arch,
            (config.max_memory, config.max_virtual_memory),
            &args,
            env,
            credentials,
        )?;
        loaded.executable = guest_executable(config, cwd, &path)?;
        return Ok(loaded);
    }
    unreachable!()
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
            tid: 1,
            credentials,
            comm: task_name(&initial.executable),
            executable: initial.executable,
            argv: initial.argv,
            env: command.env.clone(),
            cpu: Engine::new(config.guest_arch, initial.entry, initial.stack),
            memory: Rc::new(RefCell::new(initial.memory)),
            heap: Rc::new(RefCell::new(Heap::new(initial.heap))),
            group: 1,
            cwd: Rc::new(RefCell::new(cwd)),
            umask: Rc::new(Cell::new(0o22)),
            clear_tid: 0,
            robust_head: 0,
            actions: Rc::new(RefCell::new([SignalAction::default(); 64])),
            signal_mask: 0,
            suspend_mask: None,
            signal_stack: blink_x86_64::SignalStack::default(),
            signals: signals::Pending::default(),
            futex: None,
            futex_syscall: None,
            poll_deadline: None,
            sleep_deadline: None,
            real_timer: Rc::new(RefCell::new(time::RealTimer::default())),
            vfork_wait: None,
            vfork_parent: None,
            fds: Rc::new(RefCell::new(BTreeMap::from([
                (0, Fd::Stdin(Rc::new(Cell::new(false)))),
                (1, Fd::Stdout(Rc::new(Cell::new(false)))),
                (2, Fd::Stderr(Rc::new(Cell::new(false)))),
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
        if process.real_timer.borrow_mut().expired() {
            let alarm = 1u64 << 13;
            if process.signal_mask & alarm != 0 {
                if let Some(target) = processes
                    .values_mut()
                    .find(|p| p.group == process.group && p.signal_mask & alarm == 0)
                {
                    target.signals.enqueue(
                        13,
                        blink_x86_64::SignalInfo {
                            code: 128,
                            ..Default::default()
                        },
                    );
                } else {
                    process.signals.enqueue(
                        13,
                        blink_x86_64::SignalInfo {
                            code: 128,
                            ..Default::default()
                        },
                    );
                }
            } else {
                process.signals.enqueue(
                    13,
                    blink_x86_64::SignalInfo {
                        code: 128,
                        ..Default::default()
                    },
                );
            }
        }
        let mut event = Event::Ready;
        // A quantum bounds cancellation latency and keeps runnable guests fair.
        for _ in 0..256 {
            if process.vfork_wait.is_some() {
                event = Event::Blocked;
                break;
            }
            if let Some(signal) = deliver_signal(&mut process, None)? {
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
                let step = process.cpu.cpu().step(&mut process.memory.borrow_mut());
                match step {
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
                    Err(error @ (Error::ArithmeticFault | Error::FloatingPointFault { .. })) => {
                        if let Some(signal) = signals::arithmetic_fault(&mut process, pc, error)? {
                            event = Event::Signal(signal);
                            break;
                        }
                        continue;
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
                        101 => 35,
                        102 => 36,
                        103 => 38,
                        113 => 228,
                        114 => 229,
                        115 => 230,
                        169 => 96,
                        25 => 72,
                        160 => 63,
                        167 => 157,
                        19 => 290,
                        20 => 291,
                        21 => 233,
                        22 => 281,
                        90 => 125,
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
                        198 => 41,
                        208 => 54,
                        209 => 55,
                        65 => 19,
                        204 => 51,
                        205 => 52,
                        199 => 53,
                        206 => 44,
                        207 => 45,
                        210 => 48,
                        203 => 42,
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
                        233 => 28,
                        216 => 25,
                        179 => 99,
                        56 => 257,
                        62 => 8,
                        61 => 217,
                        17 => 79,
                        49 => 80,
                        50 => 81,
                        51 => 161,
                        34 => 258,
                        35 => 263,
                        36 => 266,
                        37 => 265,
                        38 => 264,
                        52 => 91,
                        54 => 260,
                        55 => 93,
                        53 => 268,
                        166 => 95,
                        88 => 280,
                        43 => 137,
                        44 => 138,
                        32 => 73,
                        82 => 74,
                        83 => 75,
                        46 => 77,
                        276 => 316,
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
                    36..=38 => time::interval(&process, nr, args)?,
                    96 | 201 | 228 | 229 => time::clock(&process, nr, args)?,
                    35 | 230 => match time::sleep(&mut process, nr, args)? {
                        Some(result) => result,
                        None => {
                            process.pending = Some((number, args));
                            event = Event::Blocked;
                            break;
                        }
                    },
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
                    125 => credentials::capget(&process, &processes, args)?,
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
                        if mode & !7 != 0 {
                            errno(22)
                        } else if nr == 439 && args[3] & !(256 | 512) != 0 {
                            return Err(Error::UnsupportedSyscall(number));
                        } else {
                            match cstring(&process.memory.borrow(), ptr) {
                                Err(_) => errno(14),
                                Ok(path) => {
                                    let cwd = match directory_cwd(
                                        config,
                                        &process,
                                        if nr == 21 { -100 } else { args[0] as i32 },
                                        &path,
                                    ) {
                                        Ok(cwd) => cwd,
                                        Err(error) => {
                                            process.cpu.cpu().set_result(error);
                                            continue;
                                        }
                                    };
                                    let absolute = virtual_fs::path(&cwd, &path);
                                    let nofollow = nr == 439 && args[3] & 256 != 0;
                                    let stat = virtual_fs::stat(
                                        config, &process, &processes, &absolute, nofollow,
                                    )
                                    .unwrap_or_else(|| {
                                        blink_host::path_stat(
                                            &config.rootfs,
                                            &cwd,
                                            std::path::Path::new(&path),
                                            nofollow,
                                            config.rootless_metadata,
                                        )
                                        .map_err(|e| blink_linux::errno(&e))
                                    });
                                    match stat {
                                        Err(error) => error,
                                        Ok(stat) => {
                                            if mode & 2 != 0
                                                && virtual_fs::filesystem(&absolute)
                                                    .is_some_and(|s| s.readonly)
                                            {
                                                errno(30)
                                            } else {
                                                access_mode(
                                                    &stat,
                                                    &process.credentials,
                                                    mode as u32,
                                                    nr == 439 && args[3] & 512 != 0,
                                                )
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
                        // AT_NO_AUTOMOUNT is a no-op in the guest filesystem.
                        if flags & !(256 | 2048 | 4096) != 0 {
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
                                        .map(|fd| fd.stat(config.rootless_metadata))
                                } else {
                                    let path = if path.is_empty() && flags & 4096 != 0 {
                                        "."
                                    } else {
                                        &path
                                    };
                                    let cwd = match directory_cwd(
                                        config,
                                        &process,
                                        if nr == 262 { args[0] as i32 } else { -100 },
                                        path,
                                    ) {
                                        Ok(cwd) => cwd,
                                        Err(error) => {
                                            process.cpu.cpu().set_result(error);
                                            continue;
                                        }
                                    };
                                    let absolute = virtual_fs::path(&cwd, path);
                                    if let Some(node) = virtual_fs::stat(
                                        config,
                                        &process,
                                        &processes,
                                        &absolute,
                                        flags & 256 != 0,
                                    ) {
                                        match node {
                                            Ok(stat) => Some(Ok(stat)),
                                            Err(error) => {
                                                process.cpu.cpu().set_result(error);
                                                continue;
                                            }
                                        }
                                    } else {
                                        Some(blink_host::path_stat(
                                            &config.rootfs,
                                            &cwd,
                                            std::path::Path::new(path),
                                            flags & 256 != 0,
                                            config.rootless_metadata,
                                        ))
                                    }
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
                    54 | 55 => match process.fds.borrow().get(&(args[0] as i32)) {
                        Some(Fd::Internet(s)) => {
                            internet::option(&process, &mut s.borrow_mut(), args, nr == 54)?
                        }
                        None => errno(9),
                        Some(Fd::Socket(_) | Fd::UnixSocket { .. }) => {
                            return Err(Error::UnsupportedSyscall(number))
                        }
                        _ => errno(88),
                    },
                    19 => {
                        let fd = process.fds.borrow().get(&(args[0] as i32)).cloned();
                        match fd.as_ref() {
                            Some(Fd::Reader(pipe)) => {
                                match pipe_readv(
                                    &mut process.memory.borrow_mut(),
                                    pipe,
                                    args[1],
                                    args[2],
                                )? {
                                    Some(result) => result,
                                    None => {
                                        process.pending = Some((number, args));
                                        event = Event::Blocked;
                                        break;
                                    }
                                }
                            }
                            Some(Fd::Internet(endpoint)) => {
                                let result = internet::vectored(
                                    &mut process.memory.borrow_mut(),
                                    &endpoint.borrow(),
                                    args[1],
                                    args[2],
                                    false,
                                )?;
                                match result {
                                    None => {
                                        process.pending = Some((number, args));
                                        event = Event::Blocked;
                                        break;
                                    }
                                    Some(result) => {
                                        if result == errno(32) && args[3] & 0x4000 == 0 {
                                            process.signals.enqueue(
                                                12,
                                                blink_x86_64::SignalInfo {
                                                    sender: process.group,
                                                    uid: process.credentials.uid,
                                                    ..Default::default()
                                                },
                                            );
                                        }
                                        result
                                    }
                                }
                            }
                            Some(Fd::Socket(endpoint)) => {
                                match socket::vectored(
                                    &mut process.memory.borrow_mut(),
                                    endpoint,
                                    args[1],
                                    args[2],
                                    false,
                                )? {
                                    Some(result) => result,
                                    None => {
                                        process.pending = Some((number, args));
                                        event = Event::Blocked;
                                        break;
                                    }
                                }
                            }
                            None => errno(9),
                            _ => return Err(Error::UnsupportedSyscall(number)),
                        }
                    }
                    51 | 52 => match process.fds.borrow().get(&(args[0] as i32)) {
                        Some(Fd::Internet(s)) => {
                            internet::name(&process, &s.borrow(), args, nr == 52)?
                        }
                        Some(Fd::Socket(_)) => socket::name(&process, args)?,
                        Some(Fd::UnixSocket { .. }) if nr == 51 => socket::name(&process, args)?,
                        Some(Fd::UnixSocket { .. }) => errno(107),
                        None => errno(9),
                        _ => errno(88),
                    },
                    53 => socket::pair(&process, args, &mut next_pipe)?,
                    48 => match process.fds.borrow().get(&(args[0] as i32)) {
                        None => errno(9),
                        Some(Fd::Internet(s)) => {
                            if args[1] > 2 {
                                errno(22)
                            } else {
                                s.borrow()
                                    .host
                                    .shutdown(args[1])
                                    .map_or_else(|e| blink_linux::errno(&e), |_| 0)
                            }
                        }
                        Some(Fd::Socket(endpoint)) => {
                            if args[1] > 2 {
                                errno(22)
                            } else {
                                endpoint.shutdown(args[1]);
                                0
                            }
                        }
                        Some(Fd::UnixSocket { .. }) => errno(107),
                        _ => errno(88),
                    },
                    44 | 45 => {
                        if args[4] != 0 || args[5] != 0 {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        let fd = process.fds.borrow().get(&(args[0] as i32)).cloned();
                        match fd.as_ref() {
                            Some(Fd::Internet(endpoint)) => {
                                let result = internet::transfer(
                                    &mut process.memory.borrow_mut(),
                                    &endpoint.borrow(),
                                    args[1],
                                    args[2],
                                    nr == 44,
                                    args[3],
                                )?;
                                match result {
                                    None => {
                                        process.pending = Some((number, args));
                                        event = Event::Blocked;
                                        break;
                                    }
                                    Some(result) => {
                                        if result == errno(32) && args[3] & 0x4000 == 0 {
                                            process.signals.enqueue(
                                                12,
                                                blink_x86_64::SignalInfo {
                                                    sender: process.group,
                                                    uid: process.credentials.uid,
                                                    ..Default::default()
                                                },
                                            );
                                        }
                                        result
                                    }
                                }
                            }
                            Some(Fd::Socket(endpoint)) => {
                                let result = socket::transfer(
                                    &mut process.memory.borrow_mut(),
                                    endpoint,
                                    args[1],
                                    args[2],
                                    nr == 44,
                                    args[3],
                                )?;
                                match result {
                                    None => {
                                        process.pending = Some((number, args));
                                        event = Event::Blocked;
                                        break;
                                    }
                                    Some(result) => {
                                        if result == errno(32) && args[3] & 0x4000 == 0 {
                                            process.signals.enqueue(
                                                12,
                                                blink_x86_64::SignalInfo {
                                                    sender: process.group,
                                                    uid: process.credentials.uid,
                                                    ..Default::default()
                                                },
                                            );
                                        }
                                        result
                                    }
                                }
                            }
                            None => errno(9),
                            Some(Fd::UnixSocket { .. }) => errno(107),
                            _ => errno(88),
                        }
                    }
                    41 => {
                        if matches!(args[0], 2 | 10) {
                            let result = internet::create(config, &process, args, &mut next_pipe)?;
                            process.cpu.cpu().set_result(result);
                            continue;
                        }
                        if args[0] != 1 || args[1] & !(0x80000 | 0x800) != 1 || args[2] != 0 {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        if let Some(fd) = process.fd() {
                            process.fds.borrow_mut().insert(
                                fd,
                                Fd::UnixSocket {
                                    inode: next_pipe,
                                    nonblock: args[1] & 0x800 != 0,
                                },
                            );
                            next_pipe += 1;
                            if args[1] & 0x80000 != 0 {
                                process.descriptor_flags.borrow_mut().insert(fd);
                            }
                            fd as u64
                        } else {
                            errno(24)
                        }
                    }
                    42 => {
                        let fds = process.fds.borrow();
                        match fds.get(&(args[0] as i32)) {
                            None => errno(9),
                            Some(Fd::Internet(s)) => {
                                match internet::connect(
                                    &process.memory.borrow(),
                                    &mut s.borrow_mut(),
                                    args,
                                )? {
                                    Some(result) => result,
                                    None => {
                                        process.pending = Some((number, args));
                                        event = Event::Blocked;
                                        break;
                                    }
                                }
                            }
                            Some(Fd::UnixSocket { .. }) => {
                                if args[2] < 3 || args[2] > 110 {
                                    errno(22)
                                } else if process
                                    .memory
                                    .borrow()
                                    .check(args[1], args[2] as usize, Access::Read)
                                    .is_err()
                                {
                                    errno(14)
                                } else {
                                    let mut bytes = vec![0; args[2] as usize];
                                    process.memory.borrow().read(
                                        args[1],
                                        &mut bytes,
                                        Access::Read,
                                    )?;
                                    if u16::from_le_bytes([bytes[0], bytes[1]]) != 1 {
                                        errno(22)
                                    } else if bytes[2] == 0 {
                                        return Err(Error::UnsupportedSyscall(number));
                                    } else {
                                        let bytes = &bytes[2..];
                                        let end = bytes
                                            .iter()
                                            .position(|b| *b == 0)
                                            .unwrap_or(bytes.len());
                                        let path =
                                            std::str::from_utf8(&bytes[..end]).map_err(|_| {
                                                Error::InvalidConfig(
                                                    "non-UTF8 Unix socket path".into(),
                                                )
                                            })?;
                                        match blink_host::resolve_from(
                                            &config.rootfs,
                                            &process.cwd.borrow(),
                                            std::path::Path::new(path),
                                            false,
                                        ) {
                                            Err(e) => blink_linux::errno(&e),
                                            Ok(path) => {
                                                use std::os::unix::fs::FileTypeExt;
                                                if std::fs::metadata(path)?.file_type().is_socket()
                                                {
                                                    return Err(Error::UnsupportedSyscall(number));
                                                }
                                                errno(111)
                                            }
                                        }
                                    }
                                }
                            }
                            _ => errno(88),
                        }
                    }
                    // Optional membarrier and io_uring are absent in this guest kernel.
                    // Libraries can select their supported fallback implementations.
                    324 | 425..=427 => errno(38),
                    213 | 284 | 290 | 291 => poll::create(&process, nr, args, &mut next_pipe)?,
                    233 => poll::control(&process, args, config.guest_arch)?,
                    232 | 281 => match poll::wait(&mut process, nr, args, config.guest_arch)? {
                        Some(result) => {
                            process.poll_deadline = None;
                            if let Some(mask) = process.suspend_mask.take() {
                                process.signal_mask = mask;
                            }
                            result
                        }
                        None => {
                            process.pending = Some((number, args));
                            event = Event::Blocked;
                            break;
                        }
                    },
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
                    40 => match send_file(&process, args, stdout, stderr)? {
                        Some(result) => {
                            if result == errno(32) {
                                process.signals.enqueue(
                                    12,
                                    blink_x86_64::SignalInfo {
                                        sender: process.group,
                                        uid: process.credentials.uid,
                                        ..Default::default()
                                    },
                                );
                            }
                            result
                        }
                        None => {
                            process.pending = Some((number, args));
                            event = Event::Blocked;
                            break;
                        }
                    },
                    16 => {
                        if process.fds.borrow().contains_key(&(args[0] as i32)) {
                            match args[1] {
                                0x541b => {
                                    let available =
                                        match process.fds.borrow().get(&(args[0] as i32)) {
                                            Some(Fd::Socket(s)) => s.receive.borrow().bytes.len(),
                                            Some(Fd::Reader(p) | Fd::Writer(p)) => {
                                                p.borrow().bytes.len()
                                            }
                                            _ => return Err(Error::UnsupportedSyscall(number)),
                                        };
                                    if process
                                        .memory
                                        .borrow_mut()
                                        .store(args[2], available as u64, 4)
                                        .is_err()
                                    {
                                        errno(14)
                                    } else {
                                        0
                                    }
                                }
                                0x5421 => {
                                    let flag = process.memory.borrow().load(args[2], 4);
                                    match flag {
                                        Err(_) => errno(14),
                                        Ok(flag) => {
                                            match process.fds.borrow().get(&(args[0] as i32)) {
                                                Some(
                                                    Fd::Stdin(status)
                                                    | Fd::Stdout(status)
                                                    | Fd::Stderr(status),
                                                ) => {
                                                    status.set(flag != 0);
                                                    0
                                                }
                                                Some(
                                                    Fd::Directory(_, status)
                                                    | Fd::File {
                                                        nonblock: status, ..
                                                    },
                                                ) => {
                                                    status.set(flag != 0);
                                                    0
                                                }
                                                Some(Fd::Reader(p)) => {
                                                    p.borrow_mut().read_nonblock = flag != 0;
                                                    0
                                                }
                                                Some(Fd::Writer(p)) => {
                                                    p.borrow_mut().write_nonblock = flag != 0;
                                                    0
                                                }
                                                Some(Fd::Counter(c)) => {
                                                    c.borrow_mut().nonblock = flag != 0;
                                                    0
                                                }
                                                Some(Fd::Internet(s)) => {
                                                    s.borrow_mut().nonblock = flag != 0;
                                                    0
                                                }
                                                Some(Fd::Socket(s)) => {
                                                    s.nonblock.set(flag != 0);
                                                    0
                                                }
                                                _ => return Err(Error::UnsupportedSyscall(number)),
                                            }
                                        }
                                    }
                                }
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
                                    Fd::Epoll(_) => 2,
                                    Fd::Counter(c) => {
                                        2 | if c.borrow().nonblock { 0x800 } else { 0 }
                                    }
                                    Fd::Virtual(v) => {
                                        let v = v.borrow();
                                        let mode = if v.read && v.write {
                                            2
                                        } else {
                                            u64::from(v.write)
                                        };
                                        mode | if matches!(
                                            v.content,
                                            VirtualContent::Directory { .. }
                                        ) {
                                            0x10000
                                        } else {
                                            0
                                        }
                                    }
                                    Fd::Directory(_, status) => {
                                        0x18000 | if status.get() { 0x800 } else { 0 }
                                    }
                                    Fd::Internet(s) => {
                                        2 | if s.borrow().nonblock { 0x800 } else { 0 }
                                    }
                                    Fd::Socket(s) => 2 | if s.nonblock.get() { 0x800 } else { 0 },
                                    Fd::UnixSocket { nonblock, .. } => {
                                        2 | if *nonblock { 0x800 } else { 0 }
                                    }
                                    Fd::Stdin(status) => {
                                        if status.get() {
                                            0x800
                                        } else {
                                            0
                                        }
                                    }
                                    Fd::Reader(p) => {
                                        if p.borrow().read_nonblock {
                                            0x800
                                        } else {
                                            0
                                        }
                                    }
                                    Fd::Writer(p) => {
                                        1 | if p.borrow().write_nonblock { 0x800 } else { 0 }
                                    }
                                    Fd::Stdout(status) | Fd::Stderr(status) => {
                                        1 | if status.get() { 0x800 } else { 0 }
                                    }
                                    Fd::File {
                                        read,
                                        write,
                                        append,
                                        nonblock,
                                        ..
                                    } => {
                                        (if *read && *write {
                                            2
                                        } else if *write {
                                            1
                                        } else {
                                            0
                                        }) | 0x8000
                                            | (if *append { 1024 } else { 0 })
                                            | (if nonblock.get() { 0x800 } else { 0 })
                                    }
                                },
                                4 => match &fd {
                                    Fd::Stdin(status) | Fd::Stdout(status) | Fd::Stderr(status) => {
                                        status.set(args[2] & 0x800 != 0);
                                        0
                                    }
                                    Fd::Directory(_, status) => {
                                        status.set(args[2] & 0x800 != 0);
                                        0
                                    }
                                    Fd::File {
                                        nonblock, append, ..
                                    } if (args[2] & 1024 != 0) == *append => {
                                        nonblock.set(args[2] & 0x800 != 0);
                                        0
                                    }
                                    Fd::Reader(p) => {
                                        p.borrow_mut().read_nonblock = args[2] & 0x800 != 0;
                                        0
                                    }
                                    Fd::Writer(p) => {
                                        p.borrow_mut().write_nonblock = args[2] & 0x800 != 0;
                                        0
                                    }
                                    Fd::Counter(c) => {
                                        c.borrow_mut().nonblock = args[2] & 0x800 != 0;
                                        0
                                    }
                                    Fd::Internet(s) => {
                                        s.borrow_mut().nonblock = args[2] & 0x800 != 0;
                                        0
                                    }
                                    Fd::Socket(s) => {
                                        s.nonblock.set(args[2] & 0x800 != 0);
                                        0
                                    }
                                    _ => return Err(Error::UnsupportedSyscall(number)),
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
                                Some(Fd::Virtual(v)) => {
                                    if !v.borrow().write {
                                        errno(9)
                                    } else {
                                        let memory = process.memory.borrow();
                                        if memory
                                            .check(args[1], args[2] as usize * 16, Access::Read)
                                            .is_err()
                                        {
                                            errno(14)
                                        } else {
                                            let mut total = 0;
                                            for n in 0..args[2] {
                                                let address = memory.load(args[1] + n * 16, 8)?;
                                                let length =
                                                    memory.load(args[1] + n * 16 + 8, 8)?;
                                                if length > 0x7ffff000
                                                    || total + length > 0x7ffff000
                                                {
                                                    total = errno(22);
                                                    break;
                                                }
                                                if memory
                                                    .check(address, length as usize, Access::Read)
                                                    .is_err()
                                                {
                                                    total = errno(14);
                                                    break;
                                                }
                                                total += length;
                                            }
                                            total
                                        }
                                    }
                                }
                                Some(Fd::Internet(endpoint)) => {
                                    let result = internet::vectored(
                                        &mut process.memory.borrow_mut(),
                                        &endpoint.borrow(),
                                        args[1],
                                        args[2],
                                        true,
                                    )?;
                                    match result {
                                        None => {
                                            process.pending = Some((number, args));
                                            event = Event::Blocked;
                                            break;
                                        }
                                        Some(result) => {
                                            if result == errno(32) {
                                                process.signals.enqueue(
                                                    12,
                                                    blink_x86_64::SignalInfo {
                                                        sender: process.group,
                                                        uid: process.credentials.uid,
                                                        ..Default::default()
                                                    },
                                                );
                                            }
                                            result
                                        }
                                    }
                                }
                                Some(Fd::Socket(endpoint)) => {
                                    let result = socket::vectored(
                                        &mut process.memory.borrow_mut(),
                                        endpoint,
                                        args[1],
                                        args[2],
                                        true,
                                    )?;
                                    match result {
                                        None => {
                                            process.pending = Some((number, args));
                                            event = Event::Blocked;
                                            break;
                                        }
                                        Some(result) => {
                                            if result == errno(32) {
                                                process.signals.enqueue(
                                                    12,
                                                    blink_x86_64::SignalInfo {
                                                        sender: process.group,
                                                        uid: process.credentials.uid,
                                                        ..Default::default()
                                                    },
                                                );
                                            }
                                            result
                                        }
                                    }
                                }
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
                                                process.signals.enqueue(
                                                    12,
                                                    blink_x86_64::SignalInfo {
                                                        sender: process.group,
                                                        uid: process.credentials.uid,
                                                        ..Default::default()
                                                    },
                                                );
                                            }
                                            result
                                        }
                                    }
                                }
                                Some(Fd::Stdout(status) | Fd::Stderr(status)) => {
                                    let sink: &mut dyn Write = if matches!(fd, Some(Fd::Stdout(_)))
                                    {
                                        &mut *stdout
                                    } else {
                                        &mut *stderr
                                    };
                                    let result = output::vectored(
                                        &process.memory.borrow(),
                                        sink,
                                        args[1],
                                        args[2],
                                    )?;
                                    match result {
                                        Some(result) => result,
                                        None if status.get() => errno(11),
                                        None => {
                                            process.pending = Some((number, args));
                                            event = Event::Blocked;
                                            break;
                                        }
                                    }
                                }
                                Some(Fd::File { write: true, .. }) => {
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
                            Some(fd) => match fd.stat(config.rootless_metadata) {
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
                                let cwd = match directory_cwd(
                                    config,
                                    &process,
                                    if nr == 267 { args[0] as i32 } else { -100 },
                                    &path,
                                ) {
                                    Ok(cwd) => cwd,
                                    Err(error) => {
                                        process.cpu.cpu().set_result(error);
                                        continue;
                                    }
                                };
                                let absolute = virtual_fs::path(&cwd, &path);
                                let target = if let Some(target) =
                                    virtual_fs::link_target(&process, &absolute)
                                {
                                    Ok(target)
                                } else {
                                    blink_host::read_link(
                                        &config.rootfs,
                                        &cwd,
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
                                2 => config.max_memory as u64,
                                9 => config.max_virtual_memory as u64,
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
                    332 | 334 => errno(38), // statx/rseq are unavailable; libc may use its documented fallback.
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
                                    || virtual_usage(
                                        processes.values().chain(std::iter::once(&process)),
                                    )
                                    .checked_add((new - old) as usize)
                                    .is_none_or(|n| n > config.max_virtual_memory)
                                {
                                    false
                                } else {
                                    process
                                        .memory
                                        .borrow_mut()
                                        .map_anonymous(old, new - old, Permissions::RW)
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
                    99 => {
                        // Report guest resource limits rather than leaking the host's
                        // RAM/swap availability into allocator sizing decisions.
                        let used =
                            memory_usage(processes.values().chain(std::iter::once(&process)));
                        let mut info = [0u8; 112];
                        info[..8].copy_from_slice(&start.elapsed().as_secs().to_le_bytes());
                        info[32..40].copy_from_slice(&(config.max_memory as u64).to_le_bytes());
                        info[40..48].copy_from_slice(
                            &(config.max_memory.saturating_sub(used) as u64).to_le_bytes(),
                        );
                        info[80..82].copy_from_slice(&((processes.len() + 1) as u16).to_le_bytes());
                        info[104..108].copy_from_slice(&1u32.to_le_bytes());
                        if process.memory.borrow_mut().write(args[0], &info).is_ok() {
                            0
                        } else {
                            errno(14)
                        }
                    }
                    25 => remap_memory(config, &process, &processes, args)?,
                    28 => {
                        if args[0] & 4095 != 0 || args[1].checked_add(4095).is_none() {
                            errno(22)
                        } else {
                            let size = args[1].div_ceil(4096) * 4096;
                            match args[2] {
                                4 => match process
                                    .memory
                                    .borrow_mut()
                                    .discard_anonymous(args[0], size)
                                {
                                    Ok(true) => 0,
                                    Ok(false) => return Err(Error::UnsupportedSyscall(number)),
                                    Err(_) => errno(12),
                                },
                                10 | 11 => {
                                    if process
                                        .memory
                                        .borrow_mut()
                                        .set_fork_inheritance(args[0], size, args[2] == 11)
                                        .is_ok()
                                    {
                                        0
                                    } else {
                                        errno(12)
                                    }
                                }
                                // Access-pattern advice changes optimization only; this
                                // interpreter has no host paging/readahead policy.
                                0..=3 => {
                                    if process.memory.borrow().mapped_in_range(args[0], size) as u64
                                        == size
                                    {
                                        0
                                    } else {
                                        errno(12)
                                    }
                                }
                                _ => return Err(Error::UnsupportedSyscall(number)),
                            }
                        }
                    }
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
                                let permissions = Permissions {
                                    read: args[2] & 1 != 0,
                                    write: args[2] & 2 != 0,
                                    execute: args[2] & 4 != 0,
                                };
                                let growth = process.memory.borrow().protection_growth(
                                    args[0],
                                    args[1],
                                    permissions,
                                );
                                match growth {
                                    Ok(growth)
                                        if memory_usage(
                                            processes.values().chain(std::iter::once(&process)),
                                        )
                                        .checked_add(growth)
                                        .is_some_and(|n| n <= config.max_memory) =>
                                    {
                                        process.memory.borrow_mut().protect(
                                            args[0],
                                            args[1],
                                            permissions,
                                        )
                                    }
                                    Ok(_) => Err(Error::ResourceLimit),
                                    Err(error) => Err(error),
                                }
                            };
                            if result.is_ok() {
                                0
                            } else {
                                errno(12)
                            }
                        }
                    }
                    127 => {
                        if args[1] > 8 {
                            errno(22)
                        } else {
                            let pending = process.signals.bits & process.signal_mask;
                            if process
                                .memory
                                .borrow_mut()
                                .write(args[0], &pending.to_le_bytes()[..args[1] as usize])
                                .is_ok()
                            {
                                0
                            } else {
                                errno(14)
                            }
                        }
                    }
                    34 | 130 => {
                        if nr == 130 && process.suspend_mask.is_none() {
                            if args[1] != 8 {
                                process.cpu.cpu().set_result(errno(22));
                                continue;
                            }
                            let mask = process.memory.borrow().load(args[0], 8);
                            let Ok(mask) = mask else {
                                process.cpu.cpu().set_result(errno(14));
                                continue;
                            };
                            process.suspend_mask = Some(process.signal_mask);
                            process.signal_mask = mask & !UNBLOCKABLE;
                        }
                        process.pending = Some((number, args));
                        event = Event::Blocked;
                        break;
                    }
                    131 => signals::sigaltstack(&mut process, args)?,
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
                                        flags: m.load(args[1] + 8, 8)? as u32 as u64,
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
                                    if new.flags
                                        & !(1
                                            | 2
                                            | 4
                                            | 0x4000000
                                            | 0x8000000
                                            | 0x10000000
                                            | 0x40000000
                                            | 0x80000000)
                                        != 0
                                    {
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
                            Engine::X86(c) => c.restore_signal_context(&process.memory.borrow()),
                            _ => return Err(Error::UnsupportedSyscall(number)),
                        };
                        match restored {
                            Ok((mask, stack)) => {
                                process.signal_mask = mask & !UNBLOCKABLE;
                                if let Ok(stack) = signals::validated_stack(stack) {
                                    process.signal_stack = stack;
                                }
                            }
                            Err(_) => {
                                event = Event::Signal(11);
                                break;
                            }
                        }
                        continue;
                    }
                    204 => {
                        let target = args[0] as u32;
                        if target != 0 && target != pid && !processes.contains_key(&target) {
                            errno(3)
                        } else if args[1] < 8 || args[1] & 7 != 0 {
                            errno(22)
                        } else if process.memory.borrow_mut().store(args[2], 1, 8).is_err() {
                            errno(14)
                        } else {
                            // One virtual scheduling CPU, independent of host topology.
                            8
                        }
                    }
                    221 => {
                        let fds = process.fds.borrow();
                        match fds.get(&(args[0] as i32)) {
                            None => errno(9),
                            Some(
                                Fd::Reader(_)
                                | Fd::Writer(_)
                                | Fd::Socket(_)
                                | Fd::Internet(_)
                                | Fd::UnixSocket { .. },
                            ) => errno(29),
                            Some(_) if (args[2] as i64) < 0 || args[3] > 5 => errno(22),
                            // Advisory cache policy has no effect on the
                            // interpreter's file semantics or guest mappings.
                            Some(_) => 0,
                        }
                    }
                    2 | 85 | 257 => {
                        let (pathptr, flags, mode) = if nr == 85 {
                            (args[0], 0x241, args[1]) // creat = O_WRONLY|O_CREAT|O_TRUNC
                        } else if nr == 2 {
                            (args[0], args[1], args[2])
                        } else {
                            (args[1], args[2], args[3])
                        };
                        let path = cstring(&process.memory.borrow(), pathptr);
                        match path {
                            Err(_) => errno(14),
                            Ok(path) => {
                                let cwd = match directory_cwd(
                                    config,
                                    &process,
                                    if nr == 257 { args[0] as i32 } else { -100 },
                                    &path,
                                ) {
                                    Ok(cwd) => cwd,
                                    Err(error) => {
                                        process.cpu.cpu().set_result(error);
                                        continue;
                                    }
                                };
                                let absolute = virtual_fs::path(&cwd, &path);
                                let path = if let Some(target) =
                                    virtual_fs::link_target(&process, &absolute)
                                {
                                    if flags & 0x20000 != 0 {
                                        process.cpu.cpu().set_result(errno(40));
                                        continue;
                                    }
                                    if target.is_absolute() {
                                        target.to_string_lossy().into_owned()
                                    } else {
                                        path
                                    }
                                } else {
                                    path
                                };
                                let absolute = virtual_fs::path(&cwd, &path);
                                if let Some(node) =
                                    virtual_fs::lookup(&process, &processes, &absolute)
                                {
                                    let result =
                                        match node.and_then(|node| virtual_fs::open(node, flags)) {
                                            Err(error) => error,
                                            Ok(node) => {
                                                if let Some(fd) = process.fd() {
                                                    process.fds.borrow_mut().insert(fd, node);
                                                    if flags & 0x80000 != 0 {
                                                        process
                                                            .descriptor_flags
                                                            .borrow_mut()
                                                            .insert(fd);
                                                    }
                                                    fd as u64
                                                } else {
                                                    errno(24)
                                                }
                                            }
                                        };
                                    process.cpu.cpu().set_result(result);
                                    continue;
                                }
                                if flags
                                    & !(3
                                        | 64
                                        | 128
                                        | 256 // O_NOCTTY: no terminal devices are exposed
                                        | 0x800 // O_NONBLOCK: retained, no effect on regular file I/O
                                        | 512
                                        | 1024
                                        | 0x8000
                                        | 0x10000
                                        | 0x20000
                                        | 0x80000)
                                    != 0
                                {
                                    return Err(Error::UnsupportedSyscall(number));
                                }
                                let existing = blink_host::path_stat(
                                    &config.rootfs,
                                    &cwd,
                                    std::path::Path::new(&path),
                                    true,
                                    config.rootless_metadata,
                                )
                                .ok();
                                if flags & 3 == 3 || flags & 512 != 0 && flags & 3 == 0 {
                                    errno(22)
                                } else if flags & (64 | 128) == (64 | 128) && existing.is_some() {
                                    errno(17)
                                } else if flags & 0x20000 != 0
                                    && existing.is_some_and(|s| s.mode & 0o170000 == 0o120000)
                                {
                                    errno(40)
                                } else if let Some(fd) = process.fd() {
                                    match open_file(
                                        config,
                                        &cwd,
                                        &path,
                                        flags,
                                        mode & !(process.umask.get() as u64),
                                        &process.credentials,
                                    ) {
                                        Ok(file) => {
                                            process.fds.borrow_mut().insert(fd, file);
                                            if flags & 0x80000 != 0 {
                                                process.descriptor_flags.borrow_mut().insert(fd);
                                            }
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
                            Some(Fd::Virtual(v)) => {
                                let mut v = v.borrow_mut();
                                match &mut v.content {
                                    VirtualContent::Bytes(cursor) => {
                                        let seek = match args[2] {
                                            0 if args[1] <= i64::MAX as u64 => {
                                                Some(SeekFrom::Start(args[1]))
                                            }
                                            1 => Some(SeekFrom::Current(args[1] as i64)),
                                            2 => Some(SeekFrom::End(args[1] as i64)),
                                            _ => None,
                                        };
                                        match seek {
                                            Some(seek) => cursor
                                                .seek(seek)
                                                .unwrap_or_else(|e| blink_linux::errno(&e)),
                                            None => errno(22),
                                        }
                                    }
                                    VirtualContent::Directory { position, .. }
                                        if args[1] == 0 && args[2] == 0 =>
                                    {
                                        *position = 0;
                                        0
                                    }
                                    _ => errno(29),
                                }
                            }
                            Some(Fd::Directory(directory, _)) => {
                                if args[2] == 1 && args[1] == 0 {
                                    directory.borrow().position
                                } else if args[2] == 0 && args[1] == 0 {
                                    directory
                                        .borrow_mut()
                                        .rewind()
                                        .map_or_else(|e| blink_linux::errno(&e), |_| 0)
                                } else {
                                    return Err(Error::UnsupportedSyscall(number));
                                }
                            }
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
                    73..=75 | 77 => {
                        let result = match process.fds.borrow().get(&(args[0] as i32)) {
                            None => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
                            Some(Fd::File { file, write, .. }) => {
                                if nr == 77 && (!write || args[1] > i64::MAX as u64) {
                                    Err(std::io::ErrorKind::InvalidInput.into())
                                } else {
                                    file_control(&file.borrow(), nr, args[1])
                                }
                            }
                            Some(Fd::Directory(d, _)) if nr != 77 => {
                                file_control(&d.borrow().file, nr, args[1])
                            }
                            _ => Err(std::io::ErrorKind::InvalidInput.into()),
                        };
                        match result {
                            Ok(()) => 0,
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => errno(9),
                            Err(e)
                                if nr == 73
                                    && args[1] & 4 == 0
                                    && e.kind() == std::io::ErrorKind::WouldBlock =>
                            {
                                process.pending = Some((number, args));
                                event = Event::Blocked;
                                break;
                            }
                            Err(e) => blink_linux::errno(&e),
                        }
                    }
                    137 | 138 => filesystem_info(config, &process, nr, args)?,
                    280 => change_times(config, &process, args)?,
                    92..=94 | 260 => change_owner(config, &process, nr, args)?,
                    217 => read_directory(&process, args)?,
                    95 => process.umask.replace(args[0] as u32 & 0o777) as u64,
                    82 | 83 | 84 | 86 | 87 | 88 | 90 | 91 | 258 | 263 | 264 | 265 | 266 | 268
                    | 316 => filesystem_mutation(config, &process, nr, args)?,
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
                    81 if args[0] as i32 == -100 => errno(9),
                    81 => match directory_cwd(config, &process, args[0] as i32, ".") {
                        Ok(cwd) => {
                            *process.cwd.borrow_mut() = cwd;
                            0
                        }
                        Err(error) => error,
                    },
                    161 => {
                        if process.credentials.euid != 0 {
                            errno(1)
                        } else {
                            match cstring(&process.memory.borrow(), args[0]) {
                                Err(_) => errno(14),
                                Ok(path) => match blink_host::resolve_from(
                                    &config.rootfs,
                                    &process.cwd.borrow(),
                                    std::path::Path::new(&path),
                                    false,
                                ) {
                                    Err(error) => blink_linux::errno(&error),
                                    Ok(path) if path == config.rootfs => 0,
                                    Ok(path) if !path.is_dir() => errno(20),
                                    _ => return Err(Error::UnsupportedSyscall(number)),
                                },
                            }
                        }
                    }
                    80 => {
                        let path = cstring(&process.memory.borrow(), args[0]);
                        let cwd = process.cwd.borrow().clone();
                        if let Ok(path) = &path {
                            let absolute = virtual_fs::path(&cwd, path);
                            if let Some(node) = virtual_fs::lookup(&process, &processes, &absolute)
                            {
                                let result = match node {
                                    Err(error) => error,
                                    Ok(node) => {
                                        if matches!(node.content, VirtualContent::Directory { .. })
                                        {
                                            *process.cwd.borrow_mut() = absolute;
                                            0
                                        } else {
                                            errno(20)
                                        }
                                    }
                                };
                                process.cpu.cpu().set_result(result);
                                continue;
                            }
                        }
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
                    157 => match args[0] {
                        15 => {
                            let mut name = [0u8; 16];
                            let mut valid = true;
                            for (index, byte) in name[..15].iter_mut().enumerate() {
                                match args[1]
                                    .checked_add(index as u64)
                                    .and_then(|a| process.memory.borrow().load(a, 1).ok())
                                {
                                    Some(value) => {
                                        *byte = value as u8;
                                        if *byte == 0 {
                                            break;
                                        }
                                    }
                                    None => {
                                        valid = false;
                                        break;
                                    }
                                }
                            }
                            if valid {
                                process.comm = name;
                                0
                            } else {
                                errno(14)
                            }
                        }
                        16 => {
                            if process
                                .memory
                                .borrow_mut()
                                .write(args[1], &process.comm)
                                .is_ok()
                            {
                                0
                            } else {
                                errno(14)
                            }
                        }
                        _ => return Err(Error::UnsupportedSyscall(number)),
                    },
                    218 => {
                        process.clear_tid = args[0];
                        pid as u64
                    }
                    110 => process.parent as u64,
                    56..=58 => {
                        let vfork = nr == 58 || (nr == 56 && args[0] == 0x4111);
                        let thread = nr == 56 && args[0] & 0x10000 != 0;
                        let flags = args[0];
                        if nr == 56
                            && ((!thread
                                && !vfork
                                && (flags & 255 != 17
                                    || flags & !(17 | 0x100000 | 0x200000 | 0x1000000 | 0x400000)
                                        != 0
                                    || args[1] != 0))
                                || (thread
                                    && (flags & 0x10f00 != 0x10f00
                                        || flags
                                            & !(0x10f00
                                                | 0x40000
                                                | 0x80000
                                                | 0x100000
                                                | 0x200000
                                                | 0x400000 // obsolete CLONE_DETACHED, ignored by Linux
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
                        let bad_tid = nr == 56
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
                                && !vfork
                                && total
                                    .checked_add(process.memory.borrow().fork_backed_bytes())
                                    .is_none_or(|n| n > config.max_memory))
                            || (!thread
                                && !vfork
                                && virtual_usage(
                                    processes.values().chain(std::iter::once(&process)),
                                )
                                .checked_add(process.memory.borrow().fork_bytes())
                                .is_none_or(|n| n > config.max_virtual_memory))
                        {
                            errno(11)
                        } else {
                            let child_pid = next_pid;
                            next_pid = next_pid.checked_add(1).ok_or(Error::ResourceLimit)?;
                            let mut child = process.clone();
                            child.tid = child_pid;
                            child.clear_tid = 0;
                            child.robust_head = 0;
                            child.signals = signals::Pending::default();
                            child.suspend_mask = None;
                            child.futex = None;
                            child.futex_syscall = None;
                            child.pending = None;
                            child.poll_deadline = None;
                            child.sleep_deadline = None;
                            child.vfork_wait = None;
                            child.vfork_parent = if vfork { Some(pid) } else { None };
                            if vfork {
                                process.vfork_wait = Some(child_pid);
                            }
                            if thread {
                                child.signal_stack = blink_x86_64::SignalStack::default();
                                child.cpu.stack(args[1]);
                                if flags & 0x80000 != 0 {
                                    child.cpu.tls(if config.guest_arch == GuestArch::X86_64 {
                                        args[4]
                                    } else {
                                        args[3]
                                    });
                                }
                            } else {
                                if !vfork {
                                    child.memory =
                                        Rc::new(RefCell::new(process.memory.borrow().fork_copy()));
                                    child.heap =
                                        Rc::new(RefCell::new(process.heap.borrow().clone()));
                                } else if nr == 56 && args[1] != 0 {
                                    child.cpu.stack(args[1]);
                                }
                                child.fds = Rc::new(RefCell::new(process.fds.borrow().clone()));
                                child.cwd = Rc::new(RefCell::new(process.cwd.borrow().clone()));
                                child.umask = Rc::new(Cell::new(process.umask.get()));
                                child.actions = Rc::new(RefCell::new(*process.actions.borrow()));
                                child.descriptor_flags = Rc::new(RefCell::new(
                                    process.descriptor_flags.borrow().clone(),
                                ));
                                child.real_timer =
                                    Rc::new(RefCell::new(time::RealTimer::default()));
                                child.group = child_pid;
                                child.parent = process.group;
                            }
                            if nr == 56 {
                                if flags & 0x100000 != 0 {
                                    process.memory.borrow_mut().store(
                                        args[2],
                                        child_pid as u64,
                                        4,
                                    )?;
                                }
                                if flags & 0x1000000 != 0 {
                                    child.memory.borrow_mut().store(
                                        child_tid,
                                        child_pid as u64,
                                        4,
                                    )?;
                                }
                                if flags & 0x200000 != 0 {
                                    child.clear_tid = child_tid;
                                }
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
                                    process.futex_syscall = Some(number);
                                    process.cpu.cpu().set_result(0);
                                    event = Event::Blocked;
                                    break;
                                }
                            }
                        } else if matches!(args[1], 3 | 4 | 131 | 132) {
                            let private = args[1] & 128 != 0;
                            if args[4] & 3 != 0
                                || (args[2] as u32 as i32) < 0
                                || (args[3] as u32 as i32) < 0
                            {
                                errno(22)
                            } else if args[0] >= 0x800000000000
                                || args[4] >= 0x800000000000
                                || !private
                                    && (process
                                        .memory
                                        .borrow()
                                        .check(args[0], 4, Access::Read)
                                        .is_err()
                                        || process
                                            .memory
                                            .borrow()
                                            .check(args[4], 4, Access::Read)
                                            .is_err())
                            {
                                errno(14)
                            } else {
                                let comparison = if args[1] & 127 == 4 {
                                    process
                                        .memory
                                        .borrow()
                                        .load(args[0], 4)
                                        .map(|value| value == args[5] as u32 as u64)
                                } else {
                                    Ok(true)
                                };
                                match comparison {
                                    Err(_) => errno(14),
                                    Ok(false) => errno(11),
                                    Ok(true) => {
                                        let woke = wake(
                                            &mut processes,
                                            &process.memory,
                                            args[0],
                                            args[2] as u32,
                                        );
                                        let mut moved = 0;
                                        for waiter in processes.values_mut() {
                                            if moved >= args[3] as u32 {
                                                break;
                                            }
                                            if Rc::ptr_eq(&waiter.memory, &process.memory)
                                                && waiter.futex == Some(args[0])
                                            {
                                                waiter.futex = Some(args[4]);
                                                moved += 1;
                                            }
                                        }
                                        woke as u64 + moved as u64
                                    }
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
                        // Linux pid_t is signed 32-bit even in the AMD64 syscall ABI.
                        let wanted = args[0] as i32 as i64;
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
                                let loaded = image(
                                    config,
                                    std::path::Path::new(&path),
                                    &process.cwd.borrow(),
                                    &argv,
                                    &env,
                                    &process.credentials,
                                );
                                match loaded {
                                    Ok(new) => {
                                        let other = memory_usage(processes.values());
                                        if other
                                            .checked_add(new.memory.backed_bytes())
                                            .is_none_or(|n| n > config.max_memory)
                                            || virtual_usage(processes.values())
                                                .checked_add(new.memory.mapped_bytes())
                                                .is_none_or(|n| n > config.max_virtual_memory)
                                        {
                                            errno(12)
                                        } else {
                                            process.cpu = Engine::new(
                                                config.guest_arch,
                                                new.entry,
                                                new.stack,
                                            );
                                            process.memory = Rc::new(RefCell::new(new.memory));
                                            process.comm = task_name(&new.executable);
                                            process.executable = new.executable;
                                            process.argv = new.argv;
                                            process.env = env.clone();
                                            process.signal_stack = blink_x86_64::SignalStack {
                                                flags: (process.signal_stack.flags & 0x80000000)
                                                    | 2,
                                                ..Default::default()
                                            };
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
                                            release_vfork(&mut process, &mut processes);
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
                        if nr == 293 && args[1] & !0x80800 != 0 {
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
                                read_nonblock: nr == 293 && args[1] & 0x800 != 0,
                                write_nonblock: nr == 293 && args[1] & 0x800 != 0,
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
                            if let Fd::Counter(counter) = &fd {
                                let result = poll::transfer(
                                    &mut process.memory.borrow_mut(),
                                    &mut counter.borrow_mut(),
                                    args[1],
                                    count,
                                    nr == 1,
                                )?;
                                match result {
                                    Some(result) => result,
                                    None => {
                                        process.pending = Some((number, args));
                                        event = Event::Blocked;
                                        break;
                                    }
                                }
                            } else if process
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
                            } else if count == 0
                                && !matches!(&fd, Fd::Socket(_) | Fd::Internet(_) | Fd::Counter(_))
                            {
                                0
                            } else {
                                match (nr, &fd) {
                                    (0, Fd::Stdin(_)) => 0,
                                    (_, Fd::Epoll(_)) => errno(22),
                                    (_, Fd::Virtual(v)) => {
                                        let mut v = v.borrow_mut();
                                        if (nr == 0 && !v.read) || (nr == 1 && !v.write) {
                                            errno(9)
                                        } else if nr == 1 {
                                            count as u64
                                        } else {
                                            let mut bytes = vec![0; count.min(65536)];
                                            let read = match &mut v.content {
                                                VirtualContent::Null => 0,
                                                VirtualContent::Zero => bytes.len(),
                                                VirtualContent::Random => {
                                                    blink_host::fill_random(&mut bytes)?;
                                                    bytes.len()
                                                }
                                                VirtualContent::Bytes(cursor) => {
                                                    cursor.read(&mut bytes)?
                                                }
                                                VirtualContent::Directory { .. } => {
                                                    process.cpu.cpu().set_result(errno(21));
                                                    continue;
                                                }
                                            };
                                            process
                                                .memory
                                                .borrow_mut()
                                                .write(args[1], &bytes[..read])?;
                                            read as u64
                                        }
                                    }
                                    (0, Fd::Directory(_, _)) => errno(21),
                                    (_, Fd::Internet(endpoint)) => {
                                        let result = internet::transfer(
                                            &mut process.memory.borrow_mut(),
                                            &endpoint.borrow(),
                                            args[1],
                                            args[2],
                                            nr == 1,
                                            0,
                                        )?;
                                        match result {
                                            None => {
                                                process.pending = Some((number, args));
                                                event = Event::Blocked;
                                                break;
                                            }
                                            Some(result) => {
                                                if result == errno(32) {
                                                    process.signals.enqueue(
                                                        12,
                                                        blink_x86_64::SignalInfo {
                                                            sender: process.group,
                                                            uid: process.credentials.uid,
                                                            ..Default::default()
                                                        },
                                                    );
                                                }
                                                result
                                            }
                                        }
                                    }
                                    (_, Fd::Socket(endpoint)) => {
                                        let result = socket::transfer(
                                            &mut process.memory.borrow_mut(),
                                            endpoint,
                                            args[1],
                                            args[2],
                                            nr == 1,
                                            0,
                                        )?;
                                        match result {
                                            None => {
                                                process.pending = Some((number, args));
                                                event = Event::Blocked;
                                                break;
                                            }
                                            Some(result) => {
                                                if result == errno(32) {
                                                    process.signals.enqueue(
                                                        12,
                                                        blink_x86_64::SignalInfo {
                                                            sender: process.group,
                                                            uid: process.credentials.uid,
                                                            ..Default::default()
                                                        },
                                                    );
                                                }
                                                result
                                            }
                                        }
                                    }
                                    (_, Fd::UnixSocket { .. }) => errno(107),
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
                                            if pipe.read_nonblock {
                                                process.cpu.cpu().set_result(errno(11));
                                                continue;
                                            }
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
                                            process.signals.enqueue(
                                                12,
                                                blink_x86_64::SignalInfo {
                                                    sender: process.group,
                                                    uid: process.credentials.uid,
                                                    ..Default::default()
                                                },
                                            );
                                            process.cpu.cpu().set_result(errno(32));
                                            continue;
                                        }
                                        let capacity = 65536 - pipe.bytes.len();
                                        if capacity == 0 || (count <= 4096 && capacity < count) {
                                            if pipe.write_nonblock {
                                                process.cpu.cpu().set_result(errno(11));
                                                continue;
                                            }
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
                                    (1, Fd::Stdout(status)) | (1, Fd::Stderr(status)) => {
                                        let sink: &mut dyn Write = if matches!(&fd, Fd::Stdout(_)) {
                                            &mut *stdout
                                        } else {
                                            &mut *stderr
                                        };
                                        let result = output::transfer(
                                            &process.memory.borrow(),
                                            sink,
                                            args[1],
                                            count,
                                        )?;
                                        match result {
                                            Some(result) => result,
                                            None if status.get() => errno(11),
                                            None => {
                                                process.pending = Some((number, args));
                                                event = Event::Blocked;
                                                break;
                                            }
                                        }
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
                                process.signals.enqueue(
                                    (signal - 1) as usize,
                                    blink_x86_64::SignalInfo {
                                        sender: process.group,
                                        uid: process.credentials.uid,
                                        ..Default::default()
                                    },
                                );
                            } else {
                                let chosen = processes
                                    .iter()
                                    .find(|(_, p)| p.group == target && p.signal_mask & bit == 0)
                                    .or_else(|| processes.iter().find(|(_, p)| p.group == target))
                                    .map(|(id, _)| *id);
                                if let Some(id) = chosen {
                                    let p = processes.get_mut(&id).unwrap();
                                    p.signals.enqueue(
                                        (signal - 1) as usize,
                                        blink_x86_64::SignalInfo {
                                            sender: process.group,
                                            uid: process.credentials.uid,
                                            ..Default::default()
                                        },
                                    );
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
                release_vfork(&mut process, &mut processes);
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
                let reap = signals::child_exited(&process, &mut processes, code, signaled);
                if !reap {
                    zombies.insert(
                        process.group,
                        Zombie {
                            parent: process.parent,
                            status: if signaled { code } else { code << 8 },
                        },
                    );
                }
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
                .all(|p| p.pending.is_some() || p.futex.is_some() || p.vfork_wait.is_some())
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
            .or_insert_with(|| p.memory.borrow().backed_bytes());
    }
    spaces.values().sum()
}
fn virtual_usage<'a>(processes: impl Iterator<Item = &'a Process>) -> usize {
    let mut spaces = BTreeMap::new();
    for process in processes {
        spaces
            .entry(Rc::as_ptr(&process.memory))
            .or_insert_with(|| process.memory.borrow().mapped_bytes());
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
            p.futex_syscall = None;
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
    credentials: &blink_linux::Credentials,
) -> std::io::Result<Fd> {
    let path = blink_host::resolve_from(
        &config.rootfs,
        cwd,
        std::path::Path::new(path),
        flags & 64 != 0,
    )?;
    if path.is_dir() && flags & (3 | 64 | 512 | 1024) == 0 {
        return Ok(Fd::Directory(
            Rc::new(RefCell::new(blink_host::Directory::open(
                path,
                config.rootfs.clone(),
            )?)),
            Rc::new(Cell::new(flags & 0x800 != 0)),
        ));
    }
    if flags & 0x10000 != 0 && !path.is_dir() {
        return Err(std::io::ErrorKind::NotADirectory.into());
    }
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
    let created = !path.exists() && flags & 64 != 0;
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
    let file = options.open(&path)?;
    if config.rootless_metadata && created {
        blink_host::metadata::Target::File(&file).created(
            path.parent().unwrap(),
            credentials.euid,
            credentials.egid,
            mode as u32,
        )?;
    }
    Ok(Fd::File {
        file: Rc::new(RefCell::new(file)),
        read,
        write,
        cloexec: flags & 0x80000 != 0,
        append: flags & 1024 != 0,
        nonblock: Rc::new(Cell::new(flags & 0x800 != 0)),
    })
}

fn deliver_signal(process: &mut Process, forced: Option<usize>) -> Result<Option<i32>> {
    loop {
        let pending =
            process.signals.bits & !process.signal_mask & forced.map_or(u64::MAX, |n| 1 << n);
        if pending == 0 {
            return Ok(None);
        }
        let n = pending.trailing_zeros() as usize;
        process.signals.bits &= !(1u64 << n);
        let signal = n + 1;
        let action = process.actions.borrow()[n];
        if action.handler == 1 || (action.handler == 0 && matches!(signal, 17 | 18 | 23 | 28)) {
            continue;
        }
        if action.handler == 0 || signal == 9 {
            return Ok(Some(signal as i32));
        }
        let pending = process.pending.take();
        let interrupted_result =
            if let Some((number, args)) = pending.filter(|(n, _)| matches!(n, 35 | 230)) {
                time::interrupt_sleep(process, number, args)
            } else {
                errno(4)
            };
        let blocked = pending
            .map(|(number, _)| number)
            .or(process.futex_syscall.take());
        process.poll_deadline = None;
        process.futex = None;
        let saved_mask = process.suspend_mask.take().unwrap_or(process.signal_mask);
        match &mut process.cpu {
            Engine::X86(cpu) => {
                if let Some(number) = blocked {
                    if action.flags & 0x10000000 != 0
                        && matches!(number, 0 | 1 | 19 | 20 | 40 | 42 | 44 | 45 | 61 | 73 | 202)
                    {
                        cpu.rip = cpu.rip.wrapping_sub(2);
                        cpu.regs[0] = number;
                    } else {
                        cpu.set_result(interrupted_result);
                    }
                }
                if cpu
                    .enter_signal_context(
                        &mut process.memory.borrow_mut(),
                        blink_x86_64::SignalContext {
                            signal: signal as u32,
                            handler: action.handler,
                            restorer: action.restorer,
                            mask: saved_mask,
                            info: process.signals.info[n],
                            stack: process.signal_stack,
                            use_stack: action.flags & 0x8000000 != 0,
                        },
                    )
                    .is_err()
                {
                    return Ok(Some(11));
                }
                if process.signal_stack.flags & 0x80000000 != 0 {
                    process.signal_stack = blink_x86_64::SignalStack::default();
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

fn remap_memory(
    config: &RuntimeConfig,
    process: &Process,
    processes: &BTreeMap<u32, Process>,
    args: [u64; 6],
) -> Result<u64> {
    let [old, old_size, new_size, flags, requested, _] = args;
    if flags & !3 != 0 {
        return Err(Error::UnsupportedSyscall(25));
    }
    if old & 4095 != 0
        || old_size == 0
        || new_size == 0
        || flags & 2 != 0 && (flags & 1 == 0 || requested & 4095 != 0)
    {
        return Ok(errno(22));
    }
    let Some(old_size) = old_size.checked_add(4095).map(|n| n & !4095) else {
        return Ok(errno(22));
    };
    let Some(new_size) = new_size.checked_add(4095).map(|n| n & !4095) else {
        return Ok(errno(22));
    };
    let memory = process.memory.borrow();
    let anonymous = match memory.remap_source(old, old_size) {
        Ok(value) => value,
        Err(_) => return Ok(errno(14)),
    };
    let fixed = flags & 2 != 0;
    let new = if fixed {
        requested
    } else if new_size <= old_size
        || memory.mapped_in_range(old + old_size, new_size - old_size) == 0
    {
        old
    } else if flags & 1 != 0 {
        match memory.find_free_range(process.heap.borrow().next_map, new_size) {
            Some(address) => address,
            None => return Ok(errno(12)),
        }
    } else {
        return Ok(errno(12));
    };
    let Some(end) = new
        .checked_add(new_size)
        .filter(|end| *end < 0x800000000000)
    else {
        return Ok(errno(12));
    };
    if new != old && old < end && new < old + old_size || fixed && new == old {
        return Ok(errno(22));
    }
    if new_size > old_size && !anonymous {
        return Err(Error::UnsupportedSyscall(25));
    }
    let replaced = if new == old {
        0
    } else {
        memory.mapped_in_range(new, new_size)
    };
    let old_backed = memory.backed_in_range(old, old_size);
    let replaced_backed = if new == old {
        0
    } else {
        memory.backed_in_range(new, new_size)
    };
    let new_backed = memory.remap_backed_bytes(old, old_size, new_size)?;
    drop(memory);
    let total = memory_usage(processes.values().chain(std::iter::once(process)));
    if total
        .checked_sub(old_backed)
        .and_then(|n| n.checked_sub(replaced_backed))
        .and_then(|n| n.checked_add(new_backed))
        .is_none_or(|n| n > config.max_memory)
    {
        return Ok(errno(12));
    }
    if virtual_usage(processes.values().chain(std::iter::once(process)))
        .checked_sub(old_size as usize)
        .and_then(|size| size.checked_sub(replaced))
        .and_then(|size| size.checked_add(new_size as usize))
        .is_none_or(|size| size > config.max_virtual_memory)
    {
        return Ok(errno(12));
    }
    match process
        .memory
        .borrow_mut()
        .remap(old, old_size, new, new_size)
    {
        Ok(()) => {
            if new != old && !fixed {
                process.heap.borrow_mut().next_map = end;
            }
            Ok(new)
        }
        Err(_) => Ok(errno(12)),
    }
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
    if args[1] > config.max_virtual_memory as u64 {
        return Ok(errno(12));
    }
    let Some(size) = args[1].checked_add(4095).map(|size| size & !4095) else {
        return Ok(errno(12));
    };
    let address = if fixed || exclusive {
        args[0]
    } else {
        let Some(address) = process
            .memory
            .borrow()
            .find_free_range(process.heap.borrow().next_map, size)
        else {
            return Ok(errno(12));
        };
        address
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
    let replaced_backed = process.memory.borrow().backed_in_range(address, size);
    let backed = if anonymous && args[2] == 0 {
        0
    } else {
        size as usize
    };
    let total = memory_usage(processes.values().chain(std::iter::once(process)));
    if total
        .checked_sub(replaced_backed)
        .and_then(|n| n.checked_add(backed))
        .is_none_or(|n| n > config.max_memory)
        || virtual_usage(processes.values().chain(std::iter::once(process)))
            .checked_sub(replaced)
            .and_then(|n| n.checked_add(size as usize))
            .is_none_or(|n| n > config.max_virtual_memory)
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
    if anonymous {
        memory.map_anonymous(address, size, permissions)?;
    } else {
        memory.map(address, size, permissions)?;
    }
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

fn pipe_readv(
    memory: &mut Memory,
    pipe: &Rc<RefCell<Pipe>>,
    address: u64,
    count: u64,
) -> Result<Option<u64>> {
    if count > 1024 {
        return Ok(Some(errno(22)));
    }
    if memory
        .check(address, count as usize * 16, Access::Read)
        .is_err()
    {
        return Ok(Some(errno(14)));
    }
    let mut vectors = Vec::with_capacity(count as usize);
    let mut total = 0u64;
    for n in 0..count {
        let base = memory.load(address + n * 16, 8)?;
        let length = memory.load(address + n * 16 + 8, 8)?;
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
    if pipe.bytes.is_empty() && pipe.writers != 0 {
        return Ok(pipe.read_nonblock.then_some(errno(11)));
    }
    let mut copied = 0;
    for (base, length) in vectors {
        let length = (length as usize).min(pipe.bytes.len());
        if length == 0 {
            continue;
        }
        if memory.check(base, length, Access::Write).is_err() {
            return Ok(Some(if copied == 0 { errno(14) } else { copied }));
        }
        let bytes: Vec<_> = pipe.bytes.drain(..length).collect();
        memory.write(base, &bytes)?;
        copied += length as u64;
    }
    Ok(Some(copied))
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
        return Ok(pipe.write_nonblock.then_some(errno(11)));
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
        Some(Fd::Counter(c)) => poll::counter_events(&c.borrow()),
        Some(Fd::Epoll(e)) => u16::from(poll::readable(&e.borrow())),
        Some(Fd::Virtual(_)) => 1 | 4 | 64 | 256,
        None => return 32, // POLLNVAL
        Some(Fd::Reader(p)) => {
            let p = p.borrow();
            (if p.bytes.is_empty() { 0 } else { 1 | 64 }) | if p.writers == 0 { 16 } else { 0 }
        }
        Some(Fd::Writer(p)) => {
            let p = p.borrow();
            (if p.bytes.len() < 65536 { 4 | 256 } else { 0 }) | if p.readers == 0 { 8 } else { 0 }
        }
        Some(Fd::Internet(s)) => internet::events(&s.borrow()),
        Some(Fd::Socket(s)) => socket::events(s),
        Some(Fd::UnixSocket { .. }) => 4 | 16 | 256,
        Some(Fd::Stdin(_)) => 1 | 64,
        Some(Fd::Stdout(_) | Fd::Stderr(_)) => 4 | 256,
        // Linux regular files are always ready, even if the following I/O
        // will fail because the descriptor's access mode does not permit it.
        Some(Fd::File { .. } | Fd::Directory(_, _)) => 1 | 4 | 64 | 256,
    };
    available & (wanted | 8 | 16 | 32)
}

fn filesystem_mutation(
    config: &RuntimeConfig,
    process: &Process,
    nr: u64,
    args: [u64; 6],
) -> Result<u64> {
    if nr == 91 {
        return Ok(match process.fds.borrow().get(&(args[0] as i32)) {
            None => errno(9),
            Some(Fd::File { file, .. }) => blink_host::metadata::Target::File(&file.borrow())
                .chmod(args[1] as u32, config.rootless_metadata)
                .map_or_else(|e| blink_linux::errno(&e), |_| 0),
            _ => return Err(Error::UnsupportedSyscall(nr)),
        });
    }
    let (operation, first, second, mode, dir1, dir2) = match nr {
        258 => (83, args[1], 0, args[2], args[0] as i32, -100),
        263 => {
            if args[2] & !512 != 0 {
                return Ok(errno(22));
            }
            (
                if args[2] == 512 { 84 } else { 87 },
                args[1],
                0,
                0,
                args[0] as i32,
                -100,
            )
        }
        264 | 316 => {
            if nr == 316 && args[4] != 0 {
                return Err(Error::UnsupportedSyscall(nr));
            }
            (82, args[1], args[3], 0, args[0] as i32, args[2] as i32)
        }
        265 => {
            if args[4] != 0 {
                return Err(Error::UnsupportedSyscall(nr));
            }
            (86, args[1], args[3], 0, args[0] as i32, args[2] as i32)
        }
        266 => (88, args[0], args[2], 0, -100, args[1] as i32),
        268 => (90, args[1], 0, args[2], args[0] as i32, -100),
        _ => (nr, args[0], args[1], args[1], -100, -100),
    };
    let first = match cstring(&process.memory.borrow(), first) {
        Ok(p) => p,
        Err(_) => return Ok(errno(14)),
    };
    let second = if matches!(operation, 82 | 86 | 88) {
        match cstring(&process.memory.borrow(), second) {
            Ok(p) => p,
            Err(_) => return Ok(errno(14)),
        }
    } else {
        String::new()
    };
    let cwd = match directory_cwd(config, process, dir1, &first) {
        Ok(cwd) => cwd,
        Err(error) => return Ok(error),
    };
    let dest_cwd = match directory_cwd(config, process, dir2, &second) {
        Ok(cwd) => cwd,
        Err(error) => return Ok(error),
    };
    for path in [
        if operation == 88 {
            None
        } else {
            Some(virtual_fs::path(&cwd, &first))
        },
        if matches!(operation, 82 | 86 | 88) {
            Some(virtual_fs::path(&dest_cwd, &second))
        } else {
            None
        },
    ]
    .into_iter()
    .flatten()
    {
        if let Some(fs) = virtual_fs::filesystem(&path) {
            return Ok(errno(if fs.readonly { 30 } else { 1 }));
        }
    }
    let result = (|| -> std::io::Result<()> {
        let root = &config.rootfs;
        if operation == 83
            && blink_host::resolve_from(root, &cwd, std::path::Path::new(&first), false).is_ok()
        {
            return Err(std::io::ErrorKind::AlreadyExists.into());
        }
        let path = if operation == 90 {
            blink_host::resolve_from(root, &cwd, std::path::Path::new(&first), false)?
        } else if operation == 88 {
            std::path::PathBuf::from(&first)
        } else {
            blink_host::entry_path(root, &cwd, std::path::Path::new(&first))?
        };
        match operation {
            83 => {
                let mode = mode as u32 & !process.umask.get();
                blink_host::mkdir(
                    &path,
                    if config.rootless_metadata {
                        mode | 0o700
                    } else {
                        mode
                    },
                )?;
                if config.rootless_metadata {
                    blink_host::metadata::Target::Path(&path).created(
                        path.parent().unwrap(),
                        process.credentials.euid,
                        process.credentials.egid,
                        mode,
                    )?;
                }
                Ok(())
            }
            84 => std::fs::remove_dir(path),
            87 => std::fs::remove_file(path),
            90 => blink_host::metadata::Target::Path(&path)
                .chmod(mode as u32, config.rootless_metadata),
            82 | 86 | 88 => {
                let dest = blink_host::entry_path(root, &dest_cwd, std::path::Path::new(&second))?;
                match operation {
                    82 => std::fs::rename(path, dest),
                    86 => std::fs::hard_link(path, dest),
                    _ => {
                        blink_host::symlink(&path, &dest)?;
                        if config.rootless_metadata {
                            let parent = blink_host::metadata::Target::Path(dest.parent().unwrap())
                                .stat(true)?;
                            blink_host::metadata::Target::Path(&dest).chown(
                                process.credentials.euid,
                                if parent.mode & 0o2000 != 0 {
                                    parent.gid
                                } else {
                                    process.credentials.egid
                                },
                                true,
                            )?;
                        }
                        Ok(())
                    }
                }
            }
            _ => unreachable!(),
        }
    })();
    Ok(result.map_or_else(|e| blink_linux::errno(&e), |_| 0))
}

fn send_file(
    process: &Process,
    args: [u64; 6],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<Option<u64>> {
    let fds = process.fds.borrow();
    let Some(input) = fds.get(&(args[1] as i32)) else {
        return Ok(Some(errno(9)));
    };
    let Some(output) = fds.get(&(args[0] as i32)) else {
        return Ok(Some(errno(9)));
    };
    let file = match input {
        Fd::File {
            file, read: true, ..
        } => file,
        Fd::File { .. } => return Ok(Some(errno(9))),
        _ => return Ok(Some(errno(22))),
    };
    match output {
        Fd::File { write: false, .. } | Fd::Reader(_) | Fd::Stdin(_) => return Ok(Some(errno(9))),
        Fd::File { append: true, .. } => return Ok(Some(errno(22))),
        Fd::File { file: out, .. } if Rc::ptr_eq(file, out) => return Ok(Some(errno(22))),
        Fd::Virtual(v) if v.borrow().write => (),
        Fd::Stdout(_) | Fd::Stderr(_) | Fd::Writer(_) | Fd::Socket(_) => (),
        Fd::UnixSocket { .. } => return Ok(Some(errno(107))),
        Fd::File { .. } => (),
        _ => return Ok(Some(errno(9))),
    }
    let explicit_offset = args[2] != 0;
    let offset = if explicit_offset {
        let memory = process.memory.borrow();
        if memory.check(args[2], 8, Access::Write).is_err() {
            return Ok(Some(errno(14)));
        }
        match memory.load(args[2], 8) {
            Ok(n) if n <= i64::MAX as u64 => n,
            Ok(_) => return Ok(Some(errno(22))),
            Err(_) => return Ok(Some(errno(14))),
        }
    } else {
        match file.borrow_mut().stream_position() {
            Ok(n) => n,
            Err(e) => return Ok(Some(blink_linux::errno(&e))),
        }
    };
    let mut count = args[3].min(65536) as usize;
    if let Fd::Socket(socket) = output {
        let pipe = socket.send.borrow();
        if count > 0 && (pipe.readers == 0 || socket.write_closed.get()) {
            return Ok(Some(errno(32)));
        }
        let capacity = 65536 - pipe.bytes.len();
        if count > 0 && capacity == 0 {
            return Ok(if socket.nonblock.get() {
                Some(errno(11))
            } else {
                None
            });
        }
        count = count.min(capacity);
    }
    if let Fd::Writer(pipe) = output {
        let pipe = pipe.borrow();
        if count > 0 && pipe.readers == 0 {
            return Ok(Some(errno(32)));
        }
        let capacity = 65536 - pipe.bytes.len();
        if count > 0 && capacity == 0 {
            return Ok(pipe.write_nonblock.then_some(errno(11)));
        }
        count = count.min(capacity);
    }
    let mut bytes = vec![0; count];
    let read = match blink_host::read_at(&file.borrow(), &mut bytes, offset) {
        Ok(n) => n,
        Err(e) => return Ok(Some(blink_linux::errno(&e))),
    };
    if read == 0 {
        return Ok(Some(0));
    }
    let wrote = match output {
        Fd::Virtual(_) => read,
        Fd::Stdout(status) | Fd::Stderr(status) => {
            let sink: &mut dyn Write = if matches!(output, Fd::Stdout(_)) {
                stdout
            } else {
                stderr
            };
            match sink.write(&bytes[..read]) {
                Ok(wrote) => wrote,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Ok(status.get().then_some(errno(11)))
                }
                Err(error) => return Err(error.into()),
            }
        }
        Fd::Socket(socket) => {
            socket.send.borrow_mut().bytes.extend(&bytes[..read]);
            read
        }
        Fd::Writer(pipe) => {
            pipe.borrow_mut().bytes.extend(&bytes[..read]);
            read
        }
        Fd::File { file, .. } => match file.borrow_mut().write(&bytes[..read]) {
            Ok(n) => n,
            Err(e) => return Ok(Some(blink_linux::errno(&e))),
        },
        _ => unreachable!(),
    };
    let offset = offset + wrote as u64;
    if explicit_offset {
        process.memory.borrow_mut().store(args[2], offset, 8)?;
    } else if let Err(e) = file.borrow_mut().seek(SeekFrom::Start(offset)) {
        return Ok(Some(blink_linux::errno(&e)));
    }
    Ok(Some(wrote as u64))
}

fn release_vfork(process: &mut Process, processes: &mut BTreeMap<u32, Process>) {
    if let Some(parent) = process.vfork_parent.take() {
        if let Some(parent) = processes.get_mut(&parent) {
            parent.vfork_wait = None;
        }
    }
}

fn read_directory(process: &Process, args: [u64; 6]) -> Result<u64> {
    let fds = process.fds.borrow();
    if let Some(Fd::Virtual(v)) = fds.get(&(args[0] as i32)) {
        let count = args[2].min(65536) as usize;
        if process
            .memory
            .borrow()
            .check(args[1], count, Access::Write)
            .is_err()
        {
            return Ok(errno(14));
        }
        let mut v = v.borrow_mut();
        let VirtualContent::Directory { entries, position } = &mut v.content else {
            return Ok(errno(20));
        };
        let mut bytes = Vec::new();
        while let Some(entry) = entries.get(*position) {
            let length = (19 + entry.name.len() + 1 + 7) & !7;
            if bytes.len() + length > count {
                if bytes.is_empty() {
                    return Ok(errno(22));
                }
                break;
            }
            append_dirent(&mut bytes, entry, *position as u64 + 1);
            *position += 1;
        }
        process.memory.borrow_mut().write(args[1], &bytes)?;
        return Ok(bytes.len() as u64);
    }
    let directory = match fds.get(&(args[0] as i32)) {
        None => return Ok(errno(9)),
        Some(Fd::Directory(d, _)) => d,
        _ => return Ok(errno(20)),
    };
    let count = args[2].min(65536) as usize;
    if process
        .memory
        .borrow()
        .check(args[1], count, Access::Write)
        .is_err()
    {
        return Ok(errno(14));
    }
    let mut directory = directory.borrow_mut();
    let mut bytes = Vec::new();
    loop {
        let entry = match directory.peek() {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(e) => {
                if bytes.is_empty() {
                    return Ok(blink_linux::errno(&e));
                }
                break;
            }
        };
        let length = (19 + entry.name.len() + 1 + 7) & !7;
        if bytes.len() + length > count {
            if bytes.is_empty() {
                return Ok(errno(22));
            }
            break;
        }
        append_dirent(&mut bytes, &entry, directory.position + 1);
        directory.advance();
    }
    process.memory.borrow_mut().write(args[1], &bytes)?;
    Ok(bytes.len() as u64)
}

fn change_owner(config: &RuntimeConfig, process: &Process, nr: u64, args: [u64; 6]) -> Result<u64> {
    if !config.rootless_metadata {
        return Err(Error::UnsupportedSyscall(nr));
    }
    let (uid, gid) = if nr == 260 {
        (args[2], args[3])
    } else {
        (args[1], args[2])
    };
    let result = if nr == 93 {
        let fds = process.fds.borrow();
        match fds.get(&(args[0] as i32)) {
            None => return Ok(errno(9)),
            Some(Fd::File { file, .. }) => blink_host::metadata::Target::File(&file.borrow())
                .chown(uid as u32, gid as u32, true),
            Some(Fd::Directory(d, _)) => blink_host::metadata::Target::File(&d.borrow().file)
                .chown(uid as u32, gid as u32, true),
            _ => return Err(Error::UnsupportedSyscall(nr)),
        }
    } else {
        let pointer = if nr == 260 { args[1] } else { args[0] };
        let path = match cstring(&process.memory.borrow(), pointer) {
            Ok(p) => p,
            Err(_) => return Ok(errno(14)),
        };
        if nr == 260 && args[4] & !256 != 0 {
            return Ok(errno(22));
        }
        let cwd = match directory_cwd(
            config,
            process,
            if nr == 260 { args[0] as i32 } else { -100 },
            &path,
        ) {
            Ok(cwd) => cwd,
            Err(error) => return Ok(error),
        };
        if let Some(fs) = virtual_fs::filesystem(&virtual_fs::path(&cwd, &path)) {
            return Ok(errno(if fs.readonly { 30 } else { 1 }));
        }
        let nofollow = nr == 94 || (nr == 260 && args[4] & 256 != 0);
        let path = if nofollow {
            blink_host::entry_path(&config.rootfs, &cwd, std::path::Path::new(&path))
        } else {
            blink_host::resolve_from(&config.rootfs, &cwd, std::path::Path::new(&path), false)
        };
        match path {
            Ok(p) => blink_host::metadata::Target::Path(&p).chown(uid as u32, gid as u32, true),
            Err(e) => Err(e),
        }
    };
    Ok(result.map_or_else(|e| blink_linux::errno(&e), |_| 0))
}

fn change_times(config: &RuntimeConfig, process: &Process, args: [u64; 6]) -> Result<u64> {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    if args[3] & !256 != 0 {
        return Err(Error::UnsupportedSyscall(280));
    }
    if args[2] != 0
        && process
            .memory
            .borrow()
            .check(args[2], 32, Access::Read)
            .is_err()
    {
        return Ok(errno(14));
    }
    let mut times = std::fs::FileTimes::new();
    let now = SystemTime::now();
    for n in 0..2 {
        let value = if args[2] == 0 {
            Some(now)
        } else {
            let memory = process.memory.borrow();
            let address = match args[2].checked_add(n * 16) {
                Some(a) => a,
                None => return Ok(errno(14)),
            };
            let (seconds, nanos) = match (memory.load(address, 8), memory.load(address + 8, 8)) {
                (Ok(s), Ok(ns)) => (s as i64, ns),
                _ => return Ok(errno(14)),
            };
            match nanos {
                1073741822 => None,
                1073741823 => Some(now),
                0..=999999999 => {
                    let seconds = if seconds >= 0 {
                        UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64))
                    } else {
                        UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs()))
                    };
                    match seconds.and_then(|s| s.checked_add(Duration::from_nanos(nanos))) {
                        Some(s) => Some(s),
                        None => return Ok(errno(22)),
                    }
                }
                _ => return Ok(errno(22)),
            }
        };
        if let Some(value) = value {
            times = if n == 0 {
                times.set_accessed(value)
            } else {
                times.set_modified(value)
            };
        }
    }
    let result = if args[1] == 0 {
        match process.fds.borrow().get(&(args[0] as i32)) {
            None => return Ok(errno(9)),
            Some(Fd::File { file, .. }) => file.borrow().set_times(times),
            Some(Fd::Directory(d, _)) => d.borrow().file.set_times(times),
            _ => return Err(Error::UnsupportedSyscall(280)),
        }
    } else {
        let path = match cstring(&process.memory.borrow(), args[1]) {
            Ok(p) => p,
            Err(_) => return Ok(errno(14)),
        };
        let cwd = match directory_cwd(config, process, args[0] as i32, &path) {
            Ok(cwd) => cwd,
            Err(error) => return Ok(error),
        };
        if let Some(fs) = virtual_fs::filesystem(&virtual_fs::path(&cwd, &path)) {
            return Ok(errno(if fs.readonly { 30 } else { 1 }));
        }
        let path = if args[3] & 256 != 0 {
            blink_host::entry_path(&config.rootfs, &cwd, std::path::Path::new(&path))
        } else {
            blink_host::resolve_from(&config.rootfs, &cwd, std::path::Path::new(&path), false)
        };
        match path {
            Err(e) => Err(e),
            Ok(path) => {
                if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                    return Err(Error::UnsupportedSyscall(280));
                }
                std::fs::File::open(path).and_then(|f| f.set_times(times))
            }
        }
    };
    Ok(result.map_or_else(|e| blink_linux::errno(&e), |_| 0))
}

fn filesystem_info(
    config: &RuntimeConfig,
    process: &Process,
    nr: u64,
    args: [u64; 6],
) -> Result<u64> {
    let virtual_path = if nr == 138 {
        match process.fds.borrow().get(&(args[0] as i32)) {
            Some(Fd::Virtual(v)) => Some(v.borrow().path.clone()),
            _ => None,
        }
    } else {
        match cstring(&process.memory.borrow(), args[0]) {
            Ok(path) => Some(virtual_fs::path(&process.cwd.borrow(), &path)),
            Err(_) => return Ok(errno(14)),
        }
    };
    if let Some(path) = virtual_path {
        if let Some(info) = virtual_fs::filesystem(&path) {
            if nr == 137 {
                if let Some(Err(error)) = virtual_fs::lookup(process, &BTreeMap::new(), &path) {
                    return Ok(error);
                }
            }
            return Ok(
                if process
                    .memory
                    .borrow_mut()
                    .write(args[1], &blink_linux::statfs_bytes(&info))
                    .is_ok()
                {
                    0
                } else {
                    errno(14)
                },
            );
        }
    }
    let info = if nr == 138 {
        match process.fds.borrow().get(&(args[0] as i32)) {
            None => return Ok(errno(9)),
            Some(Fd::File { file, .. }) => blink_host::filesystem_stat(&file.borrow()),
            Some(Fd::Directory(d, _)) => blink_host::filesystem_stat(&d.borrow().file),
            _ => return Err(Error::UnsupportedSyscall(nr)),
        }
    } else {
        let path = match cstring(&process.memory.borrow(), args[0]) {
            Ok(p) => p,
            Err(_) => return Ok(errno(14)),
        };
        blink_host::resolve_from(
            &config.rootfs,
            &process.cwd.borrow(),
            std::path::Path::new(&path),
            false,
        )
        .and_then(std::fs::File::open)
        .and_then(|f| blink_host::filesystem_stat(&f))
    };
    Ok(match info {
        Err(e) => blink_linux::errno(&e),
        Ok(info) => {
            if process
                .memory
                .borrow_mut()
                .write(args[1], &blink_linux::statfs_bytes(&info))
                .is_ok()
            {
                0
            } else {
                errno(14)
            }
        }
    })
}

fn directory_cwd(
    config: &RuntimeConfig,
    process: &Process,
    fd: i32,
    path: &str,
) -> std::result::Result<std::path::PathBuf, u64> {
    if path.starts_with('/') {
        return Ok(std::path::PathBuf::from("/"));
    }
    if fd == -100 {
        return Ok(process.cwd.borrow().clone());
    }
    match process.fds.borrow().get(&fd) {
        None => Err(errno(9)),
        Some(Fd::Virtual(v)) => {
            let v = v.borrow();
            if matches!(v.content, VirtualContent::Directory { .. }) {
                Ok(v.path.clone())
            } else {
                Err(errno(20))
            }
        }
        Some(Fd::Directory(directory, _)) => directory
            .borrow()
            .path
            .strip_prefix(&config.rootfs)
            .map(|p| std::path::Path::new("/").join(p))
            .map_err(|_| errno(2)),
        _ => Err(errno(20)),
    }
}

fn file_control(file: &std::fs::File, nr: u64, value: u64) -> std::io::Result<()> {
    match nr {
        73 => blink_host::flock(file, value),
        74 => file.sync_all(),
        75 => file.sync_data(),
        77 => file.set_len(value),
        _ => unreachable!(),
    }
}

fn append_dirent(bytes: &mut Vec<u8>, entry: &blink_host::DirectoryEntry, next: u64) {
    let length = (19 + entry.name.len() + 1 + 7) & !7;
    let offset = bytes.len();
    bytes.resize(offset + length, 0);
    bytes[offset..offset + 8].copy_from_slice(&entry.inode.to_le_bytes());
    bytes[offset + 8..offset + 16].copy_from_slice(&next.to_le_bytes());
    bytes[offset + 16..offset + 18].copy_from_slice(&(length as u16).to_le_bytes());
    bytes[offset + 18] = entry.kind;
    bytes[offset + 19..offset + 19 + entry.name.len()].copy_from_slice(&entry.name);
}

fn access_mode(
    stat: &blink_host::FileStat,
    credentials: &blink_linux::Credentials,
    mode: u32,
    effective: bool,
) -> u64 {
    let (uid, gid) = if effective {
        (credentials.euid, credentials.egid)
    } else {
        (credentials.uid, credentials.gid)
    };
    if uid == 0 {
        return if mode & 1 != 0 && stat.mode & 0o170000 != 0o040000 && stat.mode & 0o111 == 0 {
            errno(13)
        } else {
            0
        };
    }
    let shift = if uid == stat.uid {
        6
    } else if gid == stat.gid || credentials.groups.contains(&stat.gid) {
        3
    } else {
        0
    };
    if (stat.mode >> shift) & mode == mode {
        0
    } else {
        errno(13)
    }
}

fn task_name(path: &std::path::Path) -> [u8; 16] {
    let mut name = [0; 16];
    let source = path.file_name().unwrap_or_default().to_string_lossy();
    let length = source.len().min(15);
    name[..length].copy_from_slice(&source.as_bytes()[..length]);
    name
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
