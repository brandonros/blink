//! Cooperative guest processes. No host fork, host signals, or blocking guest pipes.
use crate::{elf, Command, RunOutcome, RuntimeConfig, Termination};
use blink_core::{Access, Cpu, Error, GuestArch, Memory, Result, Step};
use blink_linux::{Fd, Pipe, SignalAction, UNBLOCKABLE};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
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
struct Process {
    parent: u32,
    cpu: Engine,
    memory: Rc<RefCell<Memory>>,
    fds: Rc<RefCell<BTreeMap<i32, Fd>>>,
    group: u32,
    cwd: Rc<RefCell<std::path::PathBuf>>,
    clear_tid: u64,
    actions: Rc<RefCell<[SignalAction; 64]>>,
    signal_mask: u64,
    pending_signals: u64,
    signal_senders: [u32; 64],
    futex: Option<u64>,
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
) -> Result<elf::Image> {
    use std::io::Read;
    let path = blink_host::resolve_from(&config.rootfs, cwd, path, false)?;
    if !std::fs::metadata(&path)?.is_file() {
        return Err(Error::InvalidConfig(
            "program must be a regular file".into(),
        ));
    }
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(config.max_image_size as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > config.max_image_size {
        return Err(Error::ResourceLimit);
    }
    elf::load(&bytes, config.guest_arch, config.max_memory, args, env)
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
    let initial = image(config, &command.program, &cwd, &args, &command.env)?;
    let mut processes = BTreeMap::from([(
        1u32,
        Process {
            parent: 0,
            cpu: Engine::new(config.guest_arch, initial.entry, initial.stack),
            memory: Rc::new(RefCell::new(initial.memory)),
            group: 1,
            cwd: Rc::new(RefCell::new(cwd)),
            clear_tid: 0,
            actions: Rc::new(RefCell::new([SignalAction::default(); 64])),
            signal_mask: 0,
            pending_signals: 0,
            signal_senders: [0; 64],
            futex: None,
            fds: Rc::new(RefCell::new(BTreeMap::from([
                (0, Fd::Stdin),
                (1, Fd::Stdout),
                (2, Fd::Stderr),
            ]))),
            pending: None,
        },
    )]);
    let mut zombies = BTreeMap::<u32, Zombie>::new();
    let mut group_status = BTreeMap::<u32, i32>::new();
    let mut next_pid = 2u32;
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
                match process.cpu.cpu().step(&mut process.memory.borrow_mut()) {
                    Ok(Step::Continue) => None,
                    Ok(Step::Syscall { number, args }) => Some((number, args)),
                    Err(Error::ArithmeticFault) => {
                        event = Event::Signal(8);
                        break;
                    }
                    Err(Error::MemoryFault { .. }) => {
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
                                if flags & !(3 | 64 | 512 | 1024 | 0x80000) != 0 {
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
                                            for action in process.actions.borrow_mut().iter_mut() {
                                                if action.handler > 1 {
                                                    *action = SignalAction::default();
                                                }
                                            }
                                            process
                                                .fds
                                                .borrow_mut()
                                                .retain(|_, fd| !fd.close_on_exec());
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
                        if nr == 293 && args[1] != 0 {
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
                                bytes: VecDeque::new(),
                                readers: 1,
                                writers: 1,
                            }));
                            let read = process.fd().unwrap();
                            process.fds.borrow_mut().insert(read, Fd::Reader(p.clone()));
                            let write = process.fd().unwrap();
                            process.fds.borrow_mut().insert(write, Fd::Writer(p));
                            process.memory.borrow_mut().store(args[0], read as u64, 4)?;
                            process
                                .memory
                                .borrow_mut()
                                .store(args[0] + 4, write as u64, 4)?;
                            0
                        }
                    }
                    3 => {
                        if process.fds.borrow_mut().remove(&(args[0] as i32)).is_some() {
                            0
                        } else {
                            errno(9)
                        }
                    }
                    32 | 33 | 292 => {
                        if nr == 292 && args[2] != 0 {
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
                                            event = Event::Signal(13);
                                            break;
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
