//! Rust-owned interpreter lifecycle. No process-global signals or host fork.
mod elf;
mod process;
pub use blink_core::GuestArch;
use blink_core::{Error, Result};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub rootfs: PathBuf,
    pub guest_arch: GuestArch,
    pub max_memory: usize,
    pub max_image_size: usize,
    pub max_instructions: u64,
}
impl RuntimeConfig {
    pub fn new(rootfs: impl Into<PathBuf>, guest_arch: GuestArch) -> Self {
        Self {
            rootfs: rootfs.into(),
            guest_arch,
            max_memory: 64 * 1024 * 1024,
            max_image_size: 32 * 1024 * 1024,
            max_instructions: 100_000_000,
        }
    }
}
#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
#[derive(Clone)]
pub struct Command {
    pub program: PathBuf,
    pub cwd: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub timeout: Option<Duration>,
    pub cancellation: CancellationToken,
}
impl Command {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            cwd: PathBuf::from("/"),
            args: Vec::new(),
            env: Vec::new(),
            timeout: None,
            cancellation: CancellationToken::default(),
        }
    }
    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.cwd = path.into();
        self
    }
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Termination {
    Exited(i32),
    Signaled(i32),
    TimedOut,
    Cancelled,
    InstructionLimit,
}
#[derive(Clone, Debug)]
pub struct RunOutcome {
    pub termination: Termination,
    pub instructions: u64,
}
impl RunOutcome {
    pub fn ensure_success(&self) -> Result<()> {
        if self.termination == Termination::Exited(0) {
            Ok(())
        } else {
            Err(Error::Io(format!(
                "guest terminated: {:?}",
                self.termination
            )))
        }
    }
}
/// A bounded sink for callers that want captured output without unbounded allocation.
pub struct BoundedOutput {
    bytes: Vec<u8>,
    limit: usize,
    pub discarded: u64,
}
impl BoundedOutput {
    pub fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            discarded: 0,
        }
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl Write for BoundedOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(self.limit - self.bytes.len());
        self.bytes.extend_from_slice(&buf[..n]);
        self.discarded = self.discarded.saturating_add((buf.len() - n) as u64);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub struct Runtime {
    config: RuntimeConfig,
    closed: bool,
}
impl Runtime {
    pub fn create(mut config: RuntimeConfig) -> Result<Self> {
        config.rootfs = fs::canonicalize(&config.rootfs)?;
        if !config.rootfs.is_dir()
            || config.max_memory < 2 * 1024 * 1024
            || config.max_image_size == 0
            || config.max_image_size == usize::MAX
            || config.max_instructions == 0
        {
            return Err(Error::InvalidConfig("root or resource limits".into()));
        }
        Ok(Self {
            config,
            closed: false,
        })
    }
    pub fn rootfs(&self) -> &Path {
        &self.config.rootfs
    }
    pub fn close(&mut self) -> Result<()> {
        self.closed = true;
        Ok(())
    }
    pub fn run(&mut self, command: Command) -> Result<RunOutcome> {
        self.run_with_io(command, &mut io::stdout().lock(), &mut io::stderr().lock())
    }
    /// Sinks must return promptly. Cancellation cannot interrupt a caller-supplied blocking Write.
    pub fn run_with_io(
        &mut self,
        command: Command,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> Result<RunOutcome> {
        if self.closed {
            return Err(Error::Closed);
        }
        process::run(&self.config, command, stdout, stderr)
    }
}
