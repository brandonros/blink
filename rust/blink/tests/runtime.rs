use blink::*;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "blink-rs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn write(&self, name: &str, data: &[u8]) {
        fs::write(self.0.join(name), data).unwrap();
    }
    fn runtime(&self, arch: GuestArch) -> Blink {
        Blink::create(RuntimeConfig::new(&self.0, arch)).unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn elf(arch: GuestArch, code: &[u8]) -> Vec<u8> {
    let mut b = vec![0; 4096];
    b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    fn set(b: &mut [u8], o: usize, n: usize, v: u64) {
        b[o..o + n].copy_from_slice(&v.to_le_bytes()[..n]);
    }
    for (o, n, v) in [
        (16, 2, 2),
        (18, 2, if arch == GuestArch::X86_64 { 62 } else { 183 }),
        (20, 4, 1),
        (24, 8, 0x401000),
        (32, 8, 64),
        (52, 2, 64),
        (54, 2, 56),
        (56, 2, 1),
        (64, 4, 1),
        (68, 4, 5),
        (72, 8, 0),
        (80, 8, 0x400000),
        (96, 8, 4096 + code.len() as u64),
        (104, 8, 4096 + code.len() as u64),
        (112, 8, 4096),
    ] {
        set(&mut b, o, n, v);
    }
    b.extend(code);
    b
}
fn xexit(code: u8) -> Vec<u8> {
    vec![0xbf, code, 0, 0, 0, 0xb8, 60, 0, 0, 0, 0x0f, 0x05]
}
fn arm(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|v| v.to_le_bytes()).collect()
}
fn capture(rt: &mut Blink, name: &str) -> (RunOutcome, Vec<u8>) {
    let mut out = BoundedOutput::new(4096);
    let result = rt
        .run_with_io(Command::new(name), &mut out, &mut std::io::sink())
        .unwrap();
    (result, out.as_bytes().to_vec())
}
#[test]
fn repeated_runs_and_separate_roots() {
    let a = Root::new();
    let b = Root::new();
    a.write("exit", &elf(GuestArch::X86_64, &xexit(7)));
    b.write("exit", &elf(GuestArch::X86_64, &xexit(42)));
    let mut ra = a.runtime(GuestArch::X86_64);
    let mut rb = b.runtime(GuestArch::X86_64);
    for _ in 0..100 {
        assert_eq!(
            capture(&mut ra, "/exit").0.termination,
            Termination::Exited(7)
        );
        assert_eq!(
            capture(&mut rb, "/exit").0.termination,
            Termination::Exited(42)
        );
    }
    ra.close().unwrap();
    ra.close().unwrap();
    assert!(matches!(ra.run(Command::new("exit")), Err(Error::Closed)));
}
#[test]
fn failure_then_success() {
    let r = Root::new();
    r.write("bad", b"bad");
    r.write("unsupported", &elf(GuestArch::X86_64, &[0x0f, 0x0b]));
    r.write("good", &elf(GuestArch::X86_64, &xexit(0)));
    let mut rt = r.runtime(GuestArch::X86_64);
    assert!(matches!(
        rt.run(Command::new("bad")),
        Err(Error::InvalidElf(_))
    ));
    assert!(matches!(
        rt.run(Command::new("unsupported")),
        Err(Error::UnsupportedInstruction { .. })
    ));
    capture(&mut rt, "good").0.ensure_success().unwrap();
}
#[test]
fn amd64_write_and_bounded_output() {
    let r = Root::new();
    let mut code = vec![0xb8, 1, 0, 0, 0, 0xbf, 1, 0, 0, 0, 0x48, 0xbe];
    let len = 12 + 8 + 5 + 2 + 12;
    code.extend((0x401000u64 + len).to_le_bytes());
    code.extend([0xba, 6, 0, 0, 0, 0x0f, 0x05]);
    code.extend(xexit(0));
    code.extend(b"hello\n");
    r.write("hello", &elf(GuestArch::X86_64, &code));
    let mut rt = r.runtime(GuestArch::X86_64);
    assert_eq!(capture(&mut rt, "hello").1, b"hello\n");
    let mut out = BoundedOutput::new(3);
    rt.run_with_io(Command::new("hello"), &mut out, &mut std::io::sink())
        .unwrap()
        .ensure_success()
        .unwrap();
    assert_eq!(out.as_bytes(), b"hel");
    assert_eq!(out.discarded, 3);
}
#[test]
fn arm64_write_and_repeat() {
    let r = Root::new();
    let mut code = arm(&[
        0xd2800020, 0x100000e1, 0xd28000c2, 0xd2800808, 0xd4000001, 0xd2800000, 0xd2800ba8,
        0xd4000001,
    ]);
    code.extend(b"hello\n");
    r.write("hello", &elf(GuestArch::Aarch64, &code));
    let mut rt = r.runtime(GuestArch::Aarch64);
    for _ in 0..10 {
        let (o, b) = capture(&mut rt, "hello");
        o.ensure_success().unwrap();
        assert_eq!(b, b"hello\n");
    }
}
#[test]
fn timeout_cancel_and_budget() {
    let r = Root::new();
    r.write("loop", &elf(GuestArch::X86_64, &[0xeb, 0xfe]));
    let mut rt = r.runtime(GuestArch::X86_64);
    let o = rt
        .run(Command::new("loop").timeout(Duration::from_millis(2)))
        .unwrap();
    assert_eq!(o.termination, Termination::TimedOut);
    let command = Command::new("loop");
    let token = command.cancellation.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2));
        token.cancel();
    });
    assert_eq!(rt.run(command).unwrap().termination, Termination::Cancelled);
    thread.join().unwrap();
    let mut config = RuntimeConfig::new(&r.0, GuestArch::X86_64);
    config.max_instructions = 10;
    let mut rt = Blink::create(config).unwrap();
    let o = rt.run(Command::new("loop")).unwrap();
    assert_eq!(o.termination, Termination::InstructionLimit);
    assert_eq!(o.instructions, 10);
}
#[test]
fn guest_fault_and_wrong_architecture() {
    let r = Root::new();
    r.write("fault", &elf(GuestArch::X86_64, &[0x48, 0x8b, 0x00]));
    r.write(
        "arm",
        &elf(
            GuestArch::Aarch64,
            &arm(&[0xd2800000, 0xd2800ba8, 0xd4000001]),
        ),
    );
    let mut rt = r.runtime(GuestArch::X86_64);
    assert_eq!(
        capture(&mut rt, "fault").0.termination,
        Termination::Signaled(11)
    );
    assert!(matches!(
        rt.run(Command::new("arm")),
        Err(Error::InvalidElf(_))
    ));
}
#[test]
fn stack_has_argc_argv_and_env() {
    let r = Root::new();
    r.write(
        "argc",
        &elf(
            GuestArch::X86_64,
            &[0x48, 0x8b, 0x3c, 0x24, 0xb8, 60, 0, 0, 0, 0x0f, 0x05],
        ),
    );
    let mut rt = r.runtime(GuestArch::X86_64);
    assert_eq!(
        rt.run(Command::new("argc").arg("one").arg("two"))
            .unwrap()
            .termination,
        Termination::Exited(3)
    );
}
#[test]
fn malformed_headers_do_not_panic() {
    let r = Root::new();
    let valid = elf(GuestArch::X86_64, &xexit(0));
    let mut rt = r.runtime(GuestArch::X86_64);
    for n in 0..120 {
        r.write("input", &valid[..n]);
        assert!(rt.run(Command::new("input")).is_err());
    }
    for offset in [24, 32, 72, 80, 96, 104, 112] {
        let mut b = valid.clone();
        b[offset..offset + 8].fill(255);
        r.write("input", &b);
        assert!(rt.run(Command::new("input")).is_err());
    }
}

#[test]
fn blocked_pipe_read_is_cancellable_and_context_recovers() {
    // pipe(rsp-16); read(read_fd, rsp-8, 1), with our own write end still open.
    let code = [
        0x48, 0x83, 0xec, 0x10, 0x48, 0x89, 0xe7, 0xb8, 22, 0, 0, 0, 0x0f, 0x05, 0x8b, 0x3c, 0x24,
        0x48, 0x8d, 0x74, 0x24, 8, 0xba, 1, 0, 0, 0, 0x31, 0xc0, 0x0f, 0x05,
    ];
    let r = Root::new();
    r.write("blocked", &elf(GuestArch::X86_64, &code));
    r.write("good", &elf(GuestArch::X86_64, &xexit(0)));
    let mut rt = r.runtime(GuestArch::X86_64);
    assert_eq!(
        rt.run(Command::new("blocked").timeout(Duration::from_millis(3)))
            .unwrap()
            .termination,
        Termination::TimedOut
    );
    let command = Command::new("blocked");
    let token = command.cancellation.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(3));
        token.cancel();
    });
    assert_eq!(rt.run(command).unwrap().termination, Termination::Cancelled);
    thread.join().unwrap();
    capture(&mut rt, "good").0.ensure_success().unwrap();
}
#[test]
fn root_exit_destroys_looping_descendant() {
    // fork; child loops forever; parent exits normally. No host process is created.
    let mut code = vec![0xb8, 57, 0, 0, 0, 0x0f, 0x05, 0x85, 0xc0, 0x74, 12];
    code.extend(xexit(0));
    code.extend([0xeb, 0xfe]);
    let r = Root::new();
    r.write("fork", &elf(GuestArch::X86_64, &code));
    let mut rt = r.runtime(GuestArch::X86_64);
    for _ in 0..20 {
        rt.run(Command::new("fork").timeout(Duration::from_millis(100)))
            .unwrap()
            .ensure_success()
            .unwrap();
    }
}
#[test]
fn arm_clone_and_wait_use_shared_scheduler() {
    // clone(SIGCHLD, 0); cbz x0, child; parent wait4(child,NULL,0,NULL); exit(0).
    let code = arm(&[
        0xd2800220, 0xd2800001, 0xd2801b88, 0xd4000001, 0xb40000e0, 0xd2800001, 0xd2800002,
        0xd2800003, 0xd2802088, 0xd4000001, 0xd2800000, 0xd2800ba8, 0xd4000001,
    ]);
    let r = Root::new();
    r.write("clone", &elf(GuestArch::Aarch64, &code));
    let mut rt = r.runtime(GuestArch::Aarch64);
    rt.run(Command::new("clone"))
        .unwrap()
        .ensure_success()
        .unwrap();
}
