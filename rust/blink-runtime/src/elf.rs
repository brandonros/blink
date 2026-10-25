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
    pub executable: std::path::PathBuf,
    pub memory: Memory,
    pub entry: u64,
    pub stack: u64,
    pub heap: u64,
}
struct Loaded {
    entry: u64,
    phaddr: Option<u64>,
    count: usize,
    heap: u64,
    base: u64,
}
fn segments(bytes: &[u8], arch: GuestArch, base: u64, memory: &mut Memory) -> Result<Loaded> {
    if bytes.get(..7) != Some(&b"\x7fELF\x02\x01\x01"[..]) {
        return Err(bad("expected little-endian ELF64"));
    }
    let kind = field(bytes, 16, 2)?;
    if !matches!(kind, 2 | 3) {
        return Err(bad("expected ET_EXEC or ET_DYN"));
    }
    let base = if kind == 3 { base } else { 0 };
    let machine = match arch {
        GuestArch::X86_64 => 62,
        GuestArch::Aarch64 => 183,
    };
    if field(bytes, 18, 2)? != machine || field(bytes, 20, 4)? != 1 || field(bytes, 52, 2)? != 64 {
        return Err(bad("wrong architecture/version/header size"));
    }
    let entry = field(bytes, 24, 8)?
        .checked_add(base)
        .ok_or_else(|| bad("entry overflow"))?;
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
    let mut phaddr = None;
    let mut heap = 0;
    for n in 0..count {
        let p = phoff + n * phsize;
        let kind = field(bytes, p, 4)?;
        if kind != 1 {
            continue;
        }
        let flags = field(bytes, p + 4, 4)?;
        let offset = field(bytes, p + 8, 8)?;
        let addr = field(bytes, p + 16, 8)?
            .checked_add(base)
            .ok_or_else(|| bad("segment base overflow"))?;
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
        heap = heap.max(
            addr.checked_add(memsz)
                .and_then(|end| end.checked_add(4095))
                .ok_or(Error::ResourceLimit)?
                & !4095,
        );
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
    Ok(Loaded {
        entry,
        phaddr,
        count,
        heap,
        base,
    })
}
pub fn interpreter(bytes: &[u8]) -> Result<Option<String>> {
    if bytes.get(..7) != Some(&b"\x7fELF\x02\x01\x01"[..]) {
        return Err(bad("expected little-endian ELF64"));
    }
    let phoff = usize::try_from(field(bytes, 32, 8)?).map_err(|_| bad("phoff overflow"))?;
    let count = field(bytes, 56, 2)? as usize;
    if field(bytes, 54, 2)? != 56 || count > 1024 {
        return Err(bad("program headers"));
    }
    let mut found = None;
    for n in 0..count {
        let p = phoff
            .checked_add(n * 56)
            .ok_or_else(|| bad("program header overflow"))?;
        if field(bytes, p, 4)? == 3 {
            if found.is_some() {
                return Err(bad("multiple interpreters"));
            }
            let offset =
                usize::try_from(field(bytes, p + 8, 8)?).map_err(|_| bad("interpreter offset"))?;
            let size = field(bytes, p + 32, 8)? as usize;
            if !(2..=4096).contains(&size) {
                return Err(bad("interpreter path length"));
            }
            let end = offset
                .checked_add(size)
                .ok_or_else(|| bad("interpreter overflow"))?;
            let path = bytes
                .get(offset..end)
                .ok_or_else(|| bad("truncated interpreter"))?;
            let nul = path
                .iter()
                .position(|b| *b == 0)
                .ok_or_else(|| bad("unterminated interpreter"))?;
            found = Some(
                std::str::from_utf8(&path[..nul])
                    .map_err(|_| bad("non-UTF8 interpreter"))?
                    .to_string(),
            );
        }
    }
    Ok(found)
}
pub fn load(
    bytes: &[u8],
    interpreter_bytes: Option<&[u8]>,
    arch: GuestArch,
    max_memory: usize,
    args: &[String],
    env: &[String],
    credentials: &blink_linux::Credentials,
) -> Result<Image> {
    let mut memory = Memory::new(max_memory);
    let main = segments(bytes, arch, 0x400000000, &mut memory)?;
    let (entry, interp_base) = if let Some(interp) = interpreter_bytes {
        if interpreter(interp)?.is_some() {
            return Err(bad("nested interpreter"));
        }
        let loaded = segments(interp, arch, 0x700000000000, &mut memory)?;
        (loaded.entry, loaded.base)
    } else {
        (main.entry, 0)
    };
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
    sp = sp.checked_sub(16).ok_or(Error::ResourceLimit)?;
    let random_address = sp;
    let mut random = [0; 16];
    blink_host::fill_random(&mut random)?;
    memory.write(sp, &random)?;
    let mut words = vec![argv.len() as u64];
    words.extend(argv);
    words.push(0);
    words.extend(envp);
    words.push(0);
    // Explicitly minimal auxv; no unimplemented CPU capability bits.
    words.extend([
        6,
        4096,
        9,
        main.entry,
        4,
        56,
        5,
        main.count as u64,
        16,
        0,
        26,
        0,
        7,
        interp_base,
    ]);
    if let Some(p) = main.phaddr {
        words.extend([3, p]);
    }
    words.extend([
        25,
        random_address,
        23,
        0,
        11,
        credentials.uid as u64,
        12,
        credentials.euid as u64,
        13,
        credentials.gid as u64,
        14,
        credentials.egid as u64,
    ]);
    words.extend([0, 0]);
    sp = sp
        .checked_sub(words.len() as u64 * 8)
        .ok_or(Error::ResourceLimit)?
        & !15;
    for (n, w) in words.iter().enumerate() {
        memory.store(sp + n as u64 * 8, *w, 8)?;
    }
    Ok(Image {
        executable: std::path::PathBuf::new(),
        memory,
        entry,
        stack: sp,
        heap: main.heap,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn executable(kind: u16) -> Vec<u8> {
        let mut bytes = vec![0; 4096];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        for (offset, width, value) in [
            (16, 2, kind as u64),
            (18, 2, 62),
            (20, 4, 1),
            (24, 8, 256),
            (32, 8, 64),
            (52, 2, 64),
            (54, 2, 56),
            (56, 2, 1),
            (64, 4, 1),
            (68, 4, 5),
            (96, 8, 4096),
            (104, 8, 4096),
            (112, 8, 4096),
        ] {
            bytes[offset..offset + width].copy_from_slice(&value.to_le_bytes()[..width]);
        }
        bytes[256] = 0xc3;
        bytes
    }
    #[test]
    fn dynamic_entry_auxv_and_credentials() {
        let main = executable(3);
        let interpreter = executable(3);
        let credentials = blink_linux::Credentials::new(1000, 1001);
        let image = load(
            &main,
            Some(&interpreter),
            GuestArch::X86_64,
            4 * 1024 * 1024,
            &["/main".into()],
            &[],
            &credentials,
        )
        .unwrap();
        assert_eq!(image.entry, 0x700000000100);
        assert_eq!(image.stack & 15, 0);
        let mut cursor = image.stack + 32; // argc, argv[0], argv NULL, env NULL
        let mut aux = std::collections::BTreeMap::new();
        loop {
            let key = image.memory.load(cursor, 8).unwrap();
            let value = image.memory.load(cursor + 8, 8).unwrap();
            if key == 0 {
                break;
            }
            aux.insert(key, value);
            cursor += 16;
        }
        assert_eq!(aux[&7], 0x700000000000);
        assert_eq!(aux[&9], 0x400000100);
        assert_eq!(aux[&3], 0x400000040);
        assert_eq!(
            [aux[&11], aux[&12], aux[&13], aux[&14]],
            [1000, 1000, 1001, 1001]
        );
        image.memory.check(aux[&25], 16, Access::Read).unwrap();
        assert!(image.memory.check(image.stack, 1, Access::Execute).is_err());
    }
    #[test]
    fn interpreter_path_requires_terminated_bounded_bytes() {
        let mut bytes = executable(3);
        bytes[56..58].copy_from_slice(&2u16.to_le_bytes());
        bytes[120..124].copy_from_slice(&3u32.to_le_bytes());
        bytes[128..136].copy_from_slice(&512u64.to_le_bytes());
        bytes[152..160].copy_from_slice(&4u64.to_le_bytes());
        bytes[512..516].copy_from_slice(b"/ld\0");
        assert_eq!(super::interpreter(&bytes).unwrap().as_deref(), Some("/ld"));
        bytes[515] = b'x';
        assert!(super::interpreter(&bytes).is_err());
        bytes[152..160].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(super::interpreter(&bytes).is_err());
    }
}
