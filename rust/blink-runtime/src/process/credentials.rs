//! Queries describe guest credentials, never host capability or task state.
use super::*;
const VERSION_1: u32 = 0x19980330;
const VERSION_2: u32 = 0x20071026;
const VERSION_3: u32 = 0x20080522;

/// The current model grants virtual root the Linux capability namespace through
/// CAP_CHECKPOINT_RESTORE. Capability mutation/securebits are not implemented;
/// supported privileged operations use the guest UID checks, never host grants.
pub(super) fn capabilities(c: &blink_linux::Credentials) -> (u64, u64) {
    let root = (1u64 << 41) - 1;
    (
        if c.euid == 0 { root } else { 0 },
        if c.uid == 0 || c.euid == 0 || c.suid == 0 {
            root
        } else {
            0
        },
    )
}

pub(super) fn capget(
    process: &Process,
    others: &BTreeMap<u32, Process>,
    args: [u64; 6],
) -> Result<u64> {
    let mut memory = process.memory.borrow_mut();
    if memory.check(args[0], 8, Access::Read).is_err() {
        return Ok(errno(14));
    }
    let version = memory.load(args[0], 4)? as u32;
    let count = match version {
        VERSION_1 => 1,
        VERSION_2 | VERSION_3 => 2,
        _ => {
            if memory.store(args[0], VERSION_3 as u64, 4).is_err() {
                return Ok(errno(14));
            }
            return Ok(if args[1] == 0 { 0 } else { errno(22) });
        }
    };
    if args[1] == 0 {
        return Ok(0);
    }
    let pid = memory.load(args[0] + 4, 4)? as u32 as i32;
    if pid < 0 {
        return Ok(errno(22));
    }
    let target = if pid == 0 || pid as u32 == process.tid {
        process
    } else {
        let Some(target) = others.get(&(pid as u32)) else {
            return Ok(errno(3));
        };
        target
    };
    let (effective, permitted) = capabilities(&target.credentials);
    let mut bytes = [0; 24];
    for n in 0..count {
        bytes[n * 12..n * 12 + 4].copy_from_slice(&((effective >> (n * 32)) as u32).to_le_bytes());
        bytes[n * 12 + 4..n * 12 + 8]
            .copy_from_slice(&((permitted >> (n * 32)) as u32).to_le_bytes());
        // Inheritable capabilities start empty in each fresh command.
    }
    Ok(if memory.write(args[1], &bytes[..count * 12]).is_ok() {
        0
    } else {
        errno(14)
    })
}
