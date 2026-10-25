//! Small shared Linux syscall slice; unsupported operations are explicit errors.
use blink_core::{Access, Error, GuestArch, Memory, Result};
use std::io::Write;
pub enum Action {
    Return(u64),
    Exit(i32),
}
pub fn dispatch(
    arch: GuestArch,
    number: u64,
    args: [u64; 6],
    memory: &Memory,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<Action> {
    let (write, exit, exit_group) = match arch {
        GuestArch::X86_64 => (1, 60, 231),
        GuestArch::Aarch64 => (64, 93, 94),
    };
    if number == exit || number == exit_group {
        return Ok(Action::Exit((args[0] & 255) as i32));
    }
    if number == write {
        let sink: &mut dyn Write = match args[0] {
            1 => &mut *stdout,
            2 => &mut *stderr,
            _ => return Ok(Action::Return((-9i64) as u64)),
        };
        let count = args[2].min(0x7ffff000) as usize;
        if memory.check(args[1], count, Access::Read).is_err() {
            return Ok(Action::Return((-14i64) as u64));
        }
        let mut buf = [0; 4096];
        let mut done = 0;
        while done < count {
            let len = (count - done).min(buf.len());
            memory.read(args[1] + done as u64, &mut buf[..len], Access::Read)?;
            sink.write_all(&buf[..len]).map_err(Error::from)?;
            done += len;
        }
        return Ok(Action::Return(done as u64));
    }
    Err(Error::UnsupportedSyscall(number))
}
