# Rust runtime migration

Goal: [issue #1](https://github.com/brandonros/blink/issues/1), with early AArch64
contract validation from [issue #2](https://github.com/brandonros/blink/issues/2).
The C implementation remains intact. This is an incomplete Rust interpreter,
not builder parity or a replacement for the existing CLI.

## Run and verify

Rust 1.83+ is required. Dependencies are pinned in Cargo.lock; the AMD64 decoder
uses yaxpeax-x86 2.2.0 through a bounded adapter. There is no C fallback.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p blink-cli -- --root / --arch x86_64 /path/to/static-guest
python3 rust/differential.py --c-blink /path/to/frozen/c-blink
python3 rust/asm-regressions.py
```

The CLI also accepts `--cwd`, `--timeout-ms`, and `--max-instructions`. With no
`--root`, relative programs resolve from the host working directory; with an
explicit root, the default guest cwd is `/`.

The differential harness compiles static assembly and freestanding C fixtures,
writes `target/differential/results.json`, and checks Rust, C interpreter, C JIT,
and native Linux AMD64 when available. It reports all mismatches, including C
baseline failures. The assembly harness runs 19 original Blink regressions and
writes `target/original-asm/results.json`; use `--cc` for a cross compiler and
`--c-blink` for additional C controls. Neither suite establishes full ISA parity.

## Current implementation

The eight crates follow the issue's layered design. `blink` exposes
`Blink::create`, `run`, `run_with_io`, and `close`. Each run owns its CPUs, address
spaces, process table, descriptors, pipes, and zombies. The cooperative scheduler
never forks the host, installs signal handlers, or executes guest code natively.
Returning or failing drops the entire guest process tree. Filesystem writes
persist while cwd, descriptors, registers, and signal state start fresh each run.

Implemented slices:

- Checked page-backed R/W/X memory and mapped-memory/process/instruction limits.
- Static little-endian ELF64 ET_EXEC, zeroed BSS, argv/env/limited auxv.
- AMD64 integer operations, addressing, flags, stack, branches, multiply/divide,
  shifts/rotates, conditional moves, atomics under cooperative scheduling, and FS.
- A small AArch64 integer/load-store/branch subset and TPIDR_EL0.
- Guest fork, exec, wait, pipes, dup/close, IDs, exit and thread-group exit.
- A constrained clone thread model sharing memory, cwd, descriptors and signal
  dispositions, TLS initialization, parent/child TID writes, clear-child-TID and
  futex WAIT/WAKE. Other clone/futex variants return explicit errors.
- AMD64 rt_sigaction/procmask, queued kill delivery, integer signal frames,
  handler-edited ucontext, and rt_sigreturn. SIGKILL cannot be blocked.
- Rooted regular-file open/read/write/seek, cwd, guest-rooted symlink resolution,
  shared open-file offsets, and regular-file close-on-exec.
- Bounded capture, timeout/cancellation (including blocked pipes/futexes), and
  explicit exit/signal/timeout/cancel/budget outcomes separate from engine errors.

CPUID exposes an identity with no optional feature bits. Unsupported opcodes,
syscalls, and flags are explicit errors, not silent approximations or fallback.
The first actual C translation is the integer signal context layout/save order;
its source retains upstream ISC notices.

## Limits and next gates

This does not run a shell, package manager, Node, Bun, or either builder image.
Dynamic loading, broad ISA/SIMD/FPU coverage, memory-management syscalls, general
VFS/procfs/rootless metadata, networking, and JIT remain. ARM64 signal frames,
threaded exec, signal altstacks/restart, queued realtime signals, job control,
automatic SIGCHLD notifications, and handlers for synchronous faults remain.
No image-build checkpoint or verifier has passed with Rust.

Memory limits count unique mapped guest pages across threads and forked processes;
allocator overhead, ELF input buffers, and host sinks are separate. Guest stdin
is EOF. Caller-provided output sinks and host regular-file I/O can block; those
host operations are not yet interruptible. Rooted paths are build bookkeeping,
not a security boundary against pathname races. File modes currently use host
permissions/umask; ownership and persistent inode metadata are not emulated.
File close uses Rust's Drop and does not yet surface late host close errors.
Non-UTF8 exec/path strings and overlapping ELF segment pages are not implemented.
No macOS execution or broader host compatibility has been validated yet.

## Decoder evaluation

`blink-x86_64::decode` keeps dependency types inside the architecture crate and
fetches bytes incrementally from checked executable memory, stopping once an
instruction decodes. It never unconditionally reads 15 guest bytes. Execution
semantics remain ours. The adapter corrects yaxpeax-x86 2.2.0's unconditional
64-bit destination for MOVSXD without REX.W, exposed by Blink's native assembly
regression and covered by a focused test.

AArch64 uses yaxpeax-arm 0.5.0 and yaxpeax-arch 0.3.3 as test references, including
100,000 deterministic encoding probes. This is not a complete coverage audit.
The three yaxpeax packages are 0BSD. Formatting/serde/default features are off.
The x86 decoder and shared reader contain unsafe internals; our adapter supplies
bounded slices to their safe interfaces. The A64 decoder file has no explicit
unsafe blocks, but its dependencies do. Our own crates forbid unsafe code.
A full dependency soundness review and SIMD/atomic/system-register coverage
matrix remain before broadening advertised capabilities.

## Reference evidence

Source baseline: `29f3fb08563a3adab59372a3c20debc69bf44671`, the builder's preserved
experimental runtime, including PR #218 changes. Builder checkout:
`02d15f5826de5cbc90895b6c973730eeb297505e`.

The preexisting C executable, config, clean-build log and hash manifest are in
`/workspace/work/blink-baseline`. The clean reference build uses the separate
`o/rust-reference` directory. On this Linux x86_64 host, Rust, C interpreter and
native Linux pass all seven new differential fixtures. C JIT fails the pipe/fork
fixture and crashes during the exec fixture, including after a clean rebuild.
Those failures remain visible; C is a comparison target, not an infallible oracle.
