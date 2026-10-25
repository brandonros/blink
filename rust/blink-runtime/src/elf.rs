use blink_core::{Access, Error, GuestArch, Memory, Permissions, Result};
fn bad(s: &str) -> Error {
    Error::InvalidElf(s.into())
}
fn field(bytes: &[u8], off: usize, n: usize) -> Result<u64> {
    let s = bytes
        .get(off..off.checked_add(n).ok_or_else(|| bad("offset overflow"))?)
        .ok_or_else(|| bad("truncated ELF"))?;
    let mut b = [0; 8];
    b[..n].copy_from_slice(s);
    Ok(u64::from_le_bytes(b))
}
pub struct Image {
    pub memory: Memory,
    pub entry: u64,
    pub stack: u64,
}
pub fn load(
    bytes: &[u8],
    arch: GuestArch,
    max_memory: usize,
    args: &[String],
    env: &[String],
) -> Result<Image> {
    if bytes.get(..7) != Some(&b"\x7fELF\x02\x01\x01"[..]) {
        return Err(bad("expected little-endian ELF64"));
    }
    if field(bytes, 16, 2)? != 2 {
        return Err(bad("only static ET_EXEC is implemented"));
    }
    let machine = match arch {
        GuestArch::X86_64 => 62,
        GuestArch::Aarch64 => 183,
    };
    if field(bytes, 18, 2)? != machine || field(bytes, 20, 4)? != 1 || field(bytes, 52, 2)? != 64 {
        return Err(bad("wrong architecture/version/header size"));
    }
    let entry = field(bytes, 24, 8)?;
    let phoff = usize::try_from(field(bytes, 32, 8)?).map_err(|_| bad("phoff overflow"))?;
    let phsize = field(bytes, 54, 2)? as usize;
    let count = field(bytes, 56, 2)? as usize;
    if phsize != 56 || count == 0 || count > 1024 {
        return Err(bad("program header count/size"));
    }
    let end = phoff
        .checked_add(count * phsize)
        .ok_or_else(|| bad("program headers overflow"))?;
    if end > bytes.len() {
        return Err(bad("truncated program headers"));
    }
    let mut memory = Memory::new(max_memory);
    let mut phaddr = None;
    for n in 0..count {
        let p = phoff + n * phsize;
        let kind = field(bytes, p, 4)?;
        if kind == 3 || kind == 2 {
            return Err(bad("dynamic loading not implemented"));
        }
        if kind != 1 {
            continue;
        }
        let flags = field(bytes, p + 4, 4)?;
        let offset = field(bytes, p + 8, 8)?;
        let addr = field(bytes, p + 16, 8)?;
        let filesz = field(bytes, p + 32, 8)?;
        let memsz = field(bytes, p + 40, 8)?;
        let align = field(bytes, p + 48, 8)?;
        if filesz > memsz
            || offset
                .checked_add(filesz)
                .is_none_or(|v| v > bytes.len() as u64)
            || addr.checked_add(memsz).is_none()
        {
            return Err(bad("segment range"));
        }
        if align > 1 && (!align.is_power_of_two() || addr % align != offset % align) {
            return Err(bad("segment alignment"));
        }
        if memsz == 0 {
            continue;
        }
        memory.map(
            addr,
            memsz,
            Permissions {
                read: flags & 4 != 0,
                write: flags & 2 != 0,
                execute: flags & 1 != 0,
            },
        )?;
        memory.initialize(addr, &bytes[offset as usize..(offset + filesz) as usize])?;
        if offset <= phoff as u64 && offset + filesz >= end as u64 {
            phaddr = Some(addr + (phoff as u64 - offset));
        }
    }
    memory
        .check(entry, 1, Access::Execute)
        .map_err(|_| bad("entry is not executable"))?;
    const TOP: u64 = 0x0000_7fff_ffff_0000;
    const STACK: u64 = 1024 * 1024;
    memory.map(TOP - STACK, STACK, Permissions::RW)?;
    let mut sp = TOP;
    fn strings(memory: &mut Memory, sp: &mut u64, values: &[String]) -> Result<Vec<u64>> {
        let mut pointers = Vec::new();
        for v in values {
            if v.as_bytes().contains(&0) {
                return Err(Error::InvalidConfig("NUL in argument/environment".into()));
            }
            *sp = sp
                .checked_sub(v.len() as u64 + 1)
                .ok_or(Error::ResourceLimit)?;
            memory.write(*sp, v.as_bytes())?;
            memory.store(*sp + v.len() as u64, 0, 1)?;
            pointers.push(*sp);
        }
        Ok(pointers)
    }
    let argv = strings(&mut memory, &mut sp, args)?;
    let envp = strings(&mut memory, &mut sp, env)?;
    let mut words = vec![argv.len() as u64];
    words.extend(argv);
    words.push(0);
    words.extend(envp);
    words.push(0);
    // Explicitly minimal auxv; no unimplemented CPU capability bits.
    words.extend([6, 4096, 9, entry, 4, 56, 5, count as u64, 16, 0, 26, 0]);
    if let Some(p) = phaddr {
        words.extend([3, p]);
    }
    words.extend([0, 0]);
    sp = sp
        .checked_sub(words.len() as u64 * 8)
        .ok_or(Error::ResourceLimit)?
        & !15;
    for (n, w) in words.iter().enumerate() {
        memory.store(sp + n as u64 * 8, *w, 8)?;
    }
    Ok(Image {
        memory,
        entry,
        stack: sp,
    })
}
