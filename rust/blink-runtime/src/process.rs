//! Cooperative guest processes. No host fork, host signals, or blocking guest pipes.
use crate::{elf, Command, RunOutcome, RuntimeConfig, Termination};
use blink_core::{Access, Cpu, Error, GuestArch, Memory, Result, Step};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    io::Write,
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
    fn cpu(&mut self) -> &mut dyn Cpu {
        match self {
            Self::X86(c) => c,
            Self::Arm(c) => c,
        }
    }
}
#[derive(Default)]
struct Pipe {
    bytes: VecDeque<u8>,
    readers: usize,
    writers: usize,
}
enum Fd {
    Stdin,
    Stdout,
    Stderr,
    Reader(Rc<RefCell<Pipe>>),
    Writer(Rc<RefCell<Pipe>>),
}
impl Clone for Fd {
    fn clone(&self) -> Self {
        match self {
            Self::Stdin => Self::Stdin,
            Self::Stdout => Self::Stdout,
            Self::Stderr => Self::Stderr,
            Self::Reader(p) => {
                p.borrow_mut().readers += 1;
                Self::Reader(p.clone())
            }
            Self::Writer(p) => {
                p.borrow_mut().writers += 1;
                Self::Writer(p.clone())
            }
        }
    }
}
impl Drop for Fd {
    fn drop(&mut self) {
        match self {
            Self::Reader(p) => p.borrow_mut().readers -= 1,
            Self::Writer(p) => p.borrow_mut().writers -= 1,
            _ => {}
        }
    }
}
#[derive(Clone)]
struct Process {
    parent: u32,
    cpu: Engine,
    memory: Memory,
    fds: BTreeMap<i32, Fd>,
    pending: Option<(u64, [u64; 6])>,
}
impl Process {
    fn fd(&self) -> Option<i32> {
        (0..1024).find(|fd| !self.fds.contains_key(fd))
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
    args: &[String],
    env: &[String],
) -> Result<elf::Image> {
    use std::io::Read;
    let path = blink_host::resolve(&config.rootfs, path)?;
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::InvalidConfig(
            "program must be a regular file".into(),
        ));
    }
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
    let initial = image(config, &command.program, &args, &command.env)?;
    let mut processes = BTreeMap::from([(
        1u32,
        Process {
            parent: 0,
            cpu: Engine::new(config.guest_arch, initial.entry, initial.stack),
            memory: initial.memory,
            fds: BTreeMap::from([(0, Fd::Stdin), (1, Fd::Stdout), (2, Fd::Stderr)]),
            pending: None,
        },
    )]);
    let mut zombies = BTreeMap::<u32, Zombie>::new();
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
            if instructions >= config.max_instructions {
                break;
            }
            let syscall = if let Some(call) = process.pending.take() {
                Some(call)
            } else {
                instructions += 1;
                match process.cpu.cpu().step(&mut process.memory) {
                    Ok(Step::Continue) => None,
                    Ok(Step::Syscall { number, args }) => Some((number, args)),
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
                        64 => 1,
                        57 => 3,
                        59 => 293,
                        23 => 32,
                        24 => 292,
                        220 => 56,
                        221 => 59,
                        260 => 61,
                        172 => 39,
                        173 => 110,
                        129 => 62,
                        93 => 60,
                        94 => 231,
                        _ => u64::MAX,
                    },
                };
                let ret = match nr {
                    60 | 231 => {
                        event = Event::Exit((args[0] & 255) as i32);
                        break;
                    }
                    39 => pid as u64,
                    110 => process.parent as u64,
                    57 | 56 => {
                        // AArch64 fork is clone(SIGCHLD, 0, ...). Threads/shared VM are not implemented.
                        if nr == 56 && (args[0] != 17 || args[1] != 0) {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        let total: usize = processes
                            .values()
                            .map(|p| p.memory.mapped_bytes())
                            .sum::<usize>()
                            + process.memory.mapped_bytes();
                        if processes.len() + zombies.len() >= 1023
                            || total
                                .checked_add(process.memory.mapped_bytes())
                                .is_none_or(|n| n > config.max_memory)
                        {
                            errno(11)
                        } else {
                            let child_pid = next_pid;
                            next_pid = next_pid.checked_add(1).ok_or(Error::ResourceLimit)?;
                            let mut child = process.clone();
                            child.parent = pid;
                            child.cpu.cpu().set_result(0);
                            child.pending = None;
                            processes.insert(child_pid, child);
                            queue.push_back(child_pid);
                            child_pid as u64
                        }
                    }
                    61 => {
                        let wanted = args[0] as i64;
                        if args[2] & !1 != 0 || args[3] != 0 || wanted == 0 || wanted < -1 {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        let matches = |id: u32, parent: u32| {
                            parent == pid && (wanted == -1 || wanted == id as i64)
                        };
                        if let Some(child) = zombies
                            .iter()
                            .find(|(id, z)| matches(**id, z.parent))
                            .map(|(id, _)| *id)
                        {
                            if args[1] != 0
                                && process
                                    .memory
                                    .store(args[1], zombies[&child].status as u64, 4)
                                    .is_err()
                            {
                                errno(14)
                            } else {
                                zombies.remove(&child);
                                child as u64
                            }
                        } else if processes.iter().any(|(id, p)| matches(*id, p.parent)) {
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
                        let values = (|| {
                            Ok::<_, Error>((
                                cstring(&process.memory, args[0])?,
                                strings(&process.memory, args[1])?,
                                strings(&process.memory, args[2])?,
                            ))
                        })();
                        match values {
                            Err(_) => errno(14),
                            Ok((path, argv, env)) => {
                                match image(config, std::path::Path::new(&path), &argv, &env) {
                                    Ok(new) => {
                                        let other: usize = processes
                                            .values()
                                            .map(|p| p.memory.mapped_bytes())
                                            .sum();
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
                                            process.memory = new.memory;
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
                        if process.memory.check(args[0], 8, Access::Write).is_err() {
                            errno(14)
                        } else if process.fds.len() > 1022 {
                            errno(24)
                        } else {
                            let p = Rc::new(RefCell::new(Pipe {
                                bytes: VecDeque::new(),
                                readers: 1,
                                writers: 1,
                            }));
                            let read = process.fd().unwrap();
                            process.fds.insert(read, Fd::Reader(p.clone()));
                            let write = process.fd().unwrap();
                            process.fds.insert(write, Fd::Writer(p));
                            process.memory.store(args[0], read as u64, 4)?;
                            process.memory.store(args[0] + 4, write as u64, 4)?;
                            0
                        }
                    }
                    3 => {
                        if process.fds.remove(&(args[0] as i32)).is_some() {
                            0
                        } else {
                            errno(9)
                        }
                    }
                    32 | 33 | 292 => {
                        if nr == 292 && args[2] != 0 {
                            return Err(Error::UnsupportedSyscall(number));
                        }
                        if let Some(fd) = process.fds.get(&(args[0] as i32)).cloned() {
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
                                process.fds.insert(dest, fd);
                                dest as u64
                            }
                        } else {
                            errno(9)
                        }
                    }
                    0 | 1 => {
                        if let Some(fd) = process.fds.get(&(args[0] as i32)) {
                            let count = args[2].min(0x7ffff000) as usize;
                            if process
                                .memory
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
                                match (nr, fd) {
                                    (0, Fd::Stdin) => 0,
                                    (0, Fd::Reader(pipe)) => {
                                        let mut pipe = pipe.borrow_mut();
                                        if pipe.bytes.is_empty() && pipe.writers != 0 {
                                            process.pending = Some((number, args));
                                            event = Event::Blocked;
                                            break;
                                        }
                                        let count = count.min(pipe.bytes.len());
                                        let bytes: Vec<_> = pipe.bytes.drain(..count).collect();
                                        process.memory.write(args[1], &bytes)?;
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
                                        process.memory.read(args[1], &mut bytes, Access::Read)?;
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
                                            process.memory.read(
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
                        } else if target != pid
                            && !processes.contains_key(&target)
                            && !zombies.contains_key(&target)
                        {
                            errno(3)
                        } else if signal == 0 {
                            0
                        } else if signal != 9 && signal != 15 {
                            return Err(Error::UnsupportedSyscall(number));
                        } else if target == pid {
                            event = Event::Signal(signal as i32);
                            break;
                        } else {
                            if let Some(victim) = processes.remove(&target) {
                                zombies.insert(
                                    target,
                                    Zombie {
                                        parent: victim.parent,
                                        status: signal as i32,
                                    },
                                );
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
                if pid == 1 {
                    break 'scheduler if signaled {
                        Termination::Signaled(code)
                    } else {
                        Termination::Exited(code)
                    };
                }
                zombies.insert(
                    pid,
                    Zombie {
                        parent: process.parent,
                        status: if signaled { code } else { code << 8 },
                    },
                );
                for p in processes.values_mut() {
                    if p.parent == pid {
                        p.parent = 1;
                    }
                }
                for z in zombies.values_mut() {
                    if z.parent == pid {
                        z.parent = 1;
                    }
                }
            }
            Event::Ready | Event::Blocked => {
                processes.insert(pid, process);
                queue.push_back(pid);
            }
        }
        if !processes.is_empty() && processes.values().all(|p| p.pending.is_some()) {
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
