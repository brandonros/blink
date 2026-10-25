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

/// Notify one eligible parent thread when the child's last thread exits.
/// Explicit SIG_IGN/SA_NOCLDWAIT reaps immediately; the default disposition does not.
pub(super) fn child_exited(
    child: &Process,
    processes: &mut BTreeMap<u32, Process>,
    status: i32,
    signaled: bool,
) -> bool {
    let bit = 1 << 16;
    let target = processes
        .iter()
        .filter(|(_, p)| p.group == child.parent)
        .min_by_key(|(_, p)| p.signal_mask & bit != 0)
        .map(|(id, _)| *id);
    let Some(parent) = target.and_then(|id| processes.get_mut(&id)) else {
        return false;
    };
    let action = parent.actions.borrow()[16];
    if action.handler != 1 && parent.signals.bits & bit == 0 {
        parent.signals.info[16] = blink_x86_64::SignalInfo {
            sender: child.group,
            uid: child.credentials.uid,
            code: if signaled { 2 } else { 1 }, // CLD_KILLED / CLD_EXITED
            status,
        };
        parent.signals.bits |= bit;
    }
    action.handler == 1 || action.flags & 2 != 0
}

#[derive(Clone)]
pub(super) struct Pending {
    pub bits: u64,
    pub info: [blink_x86_64::SignalInfo; 64],
}
impl Default for Pending {
    fn default() -> Self {
        Self {
            bits: 0,
            info: [blink_x86_64::SignalInfo::default(); 64],
        }
    }
}
impl Pending {
    pub fn enqueue(&mut self, index: usize, info: blink_x86_64::SignalInfo) {
        let bit = 1u64 << index;
        // Standard signals coalesce and retain the first pending event's info.
        if self.bits & bit == 0 {
            self.info[index] = info;
            self.bits |= bit;
        }
    }
}
