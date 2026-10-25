//! Guest alternate signal-stack configuration. No host signal stack is changed.
use super::*;
use blink_x86_64::SignalStack;

pub(super) fn validated_stack(mut stack: SignalStack) -> std::result::Result<SignalStack, i64> {
    let mode = stack.flags & !0x80000000;
    if mode > 2 {
        return Err(22);
    }
    if mode == 2 {
        return Ok(SignalStack {
            flags: stack.flags,
            ..Default::default()
        });
    }
    if stack.size < 2048 {
        return Err(12);
    }
    stack.flags &= !1;
    Ok(stack)
}

pub(super) fn sigaltstack(process: &mut Process, args: [u64; 6]) -> Result<u64> {
    let Engine::X86(cpu) = &process.cpu else {
        return Err(Error::UnsupportedSyscall(131));
    };
    let old = process.signal_stack;
    let status = old.status(cpu.regs[4]);
    let mut memory = process.memory.borrow_mut();
    let new = if args[0] != 0 {
        if memory.check(args[0], 24, Access::Read).is_err() {
            return Ok(errno(14));
        }
        if status == 1 {
            return Ok(errno(1));
        }
        match validated_stack(SignalStack {
            pointer: memory.load(args[0], 8)?,
            flags: memory.load(args[0] + 8, 4)? as u32,
            size: memory.load(args[0] + 16, 8)?,
        }) {
            Ok(stack) => Some(stack),
            Err(error) => return Ok(errno(error)),
        }
    } else {
        None
    };
    if args[1] != 0 {
        let mut bytes = [0; 24];
        bytes[..8].copy_from_slice(&old.pointer.to_le_bytes());
        bytes[8..12].copy_from_slice(&status.to_le_bytes());
        bytes[16..].copy_from_slice(&old.size.to_le_bytes());
        if memory.write(args[1], &bytes).is_err() {
            return Ok(errno(14));
        }
    }
    if let Some(new) = new {
        process.signal_stack = new;
    }
    Ok(0)
}
