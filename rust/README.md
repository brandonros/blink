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
python3 rust/alpine-smoke.py --root /path/to/extracted/alpine-amd64
python3 rust/builder-smoke.py --root /path/to/alpine --builder /path/to/linux-oci-builder
python3 rust/rootless-smoke.py --root /path/to/alpine
python3 rust/apk-smoke.py --root /path/to/alpine
```

The CLI also accepts `--cwd`, `--uid`, `--gid`, repeated `--env KEY=VALUE`, `--timeout-ms`, `--max-instructions`, `--rootless-metadata`, and `--trace`. With no
`--root`, relative programs resolve from the host working directory; with an
explicit root, the default guest cwd is `/`.

The differential harness compiles static assembly and freestanding C fixtures,
writes `target/differential/results.json`, and checks Rust, C interpreter, C JIT,
and native Linux AMD64 when available. It reports all mismatches, including C
baseline failures. The assembly harness runs 22 original Blink regressions and
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
- Little-endian ELF64 ET_EXEC/ET_DYN and guest PT_INTERP loading, zeroed BSS,
  argv/env/auxv including virtual IDs, interpreter base, and host-sourced entropy.
- brk, private anonymous/file mmap, munmap/mprotect, and SIGBUS for wholly
  past-EOF mapped file pages. Dynamic relocation runs inside the guest loader.
- AMD64 integer operations, addressing, flags, stack, branches, multiply/divide,
  shifts/rotates, conditional moves, atomics under cooperative scheduling, and FS.
  A limited SSE integer/move subset supports the tested libc and musl paths.
- A small AArch64 integer/load-store/branch subset and TPIDR_EL0.
- Guest fork/vfork, exec (including bounded shebang chains), wait, pipes,
  dup/close, IDs, exit and thread-group exit. vfork shares guest memory and
  suspends only the guest caller until child exec/exit.
- A constrained clone thread model sharing memory, cwd, descriptors and signal
  dispositions, TLS initialization, parent/child TID writes, clear-child-TID and
  futex WAIT/WAKE. Other clone/futex variants return explicit errors.
- AMD64 rt_sigaction/procmask, queued kill delivery, integer/XMM signal frames,
  handler-edited ucontext, rt_sigreturn, and SA_RESTART for supported blocking
  I/O/wait/futex calls. SIGKILL cannot be blocked.
- Rooted regular-file open/read/write/seek, cwd, guest-rooted symlink resolution,
  shared open-file offsets, pread/writev, stat layouts, access/readlink, descriptor
  flags/duplication/close-on-exec, poll readiness, and bounded pipe vector writes.
  sendfile, directories/getdents64, mkdir/link/symlink/rename/unlink/chmod,
  per-process umask, regular-file/directory timestamp updates, filesystem
  information, nonblocking host-backed flock with guest scheduling, and sync.
- Virtual `/dev/null`, zero and random devices, and a small read-only proc
  namespace: guest status, exec arguments/environment, mounts, and executable
  symlink. No host proc data or host mount operations are used.
- Optional persistent rootless owner/mode attributes (`RuntimeConfig::rootless_metadata`),
  with hardlink/rename/open-descriptor identity, setgid-directory inheritance,
  and chown set-ID clearing. Host ownership stays unchanged. Attribute payloads
  match C on Darwin; Linux uses the required `user.` namespace.
- Unconnected guest Unix-domain sockets can resolve pathname connect attempts
  and report missing/non-socket targets. No host socket is opened.
- Bounded capture, timeout/cancellation (including blocked pipes/futexes), and
  explicit exit/signal/timeout/cancel/budget outcomes separate from engine errors.

CPUID exposes an identity with no optional feature bits. Unsupported opcodes,
syscalls, and flags are explicit errors, not silent approximations or fallback.
The first actual C translation is the integer signal context layout/save order;
its source retains upstream ISC notices.

## Limits and next gates

Alpine 3.22.7 AMD64 musl/BusyBox passes four real shell cases: echo, a pipeline
with external exec, file redirection/append, and command substitution. The smoke
harness uses disposable root copies and also passes with the C interpreter.
Static glibc hello passes; this host's dynamic glibc refuses the conservative
CPUID capability set. APK version, installed-package listing, offline simulation, and a persisted
world update for an already-installed package pass `apk-smoke.py`. The fixture
uses no repositories or downloads and does not establish new-package installation.
Node, Bun, and builder images are unproven.

The six small builder examples `00-hello` through `05-environment` pass every
RUN and CMD via `builder-smoke.py`, including gzip/tar round trip and SHA-256.
The `07-proc` example also passes against the virtual guest proc filesystem.
This harness reads their original Dockerfiles, retains a disposable root between
steps, and checks expected output. It does not assemble/export an OCI image or
replace the 26 agent-platform checkpoint receipts.
Broad ISA/SIMD/FPU coverage, shared file mappings, remaining directory-relative
*at operations, broader VFS/procfs, networking, and JIT remain. ARM64 signal frames,
threaded exec, signal altstacks, queued realtime signals, job control,
automatic SIGCHLD notifications, and handlers for synchronous faults remain.
No image-build checkpoint or verifier has passed with Rust.

Memory limits count unique mapped guest pages across threads and forked processes;
allocator overhead, ELF input buffers, and host sinks are separate. Guest stdin
is EOF. Caller-provided output sinks and host regular-file I/O can block; those
host operations are not yet interruptible. Rooted paths are build bookkeeping,
not a security boundary against pathname races. Without rootless metadata mode, file modes/ownership come from the host and
chown is unsupported. Rootless mode is privileged build bookkeeping, not a
permission/capability enforcement model. It retains host read/write access when
guest mode is 000. Xattr failures/corruption are errors. Linux symlink ownership
changes are not supported by user xattrs; symlink timestamps and guest xattr
syscalls remain. Directory-relative operations and rewind still use a stored path, so renaming
an open directory is not fully supported. Virtual proc files expose a small
explicit subset and snapshot exec inputs; they are not complete Linux procfs.
File close uses Rust's Drop and does not yet surface late host close errors.
Non-UTF8 exec/path strings and overlapping ELF segment pages are not implemented.
No macOS execution or broader host compatibility has been validated yet.

## Decoder evaluation

`blink-x86_64::decode` keeps dependency types inside the architecture crate and
fetches bytes incrementally from checked executable memory, stopping once an
instruction decodes. It never unconditionally reads 15 guest bytes. Execution
semantics remain ours. The adapter corrects yaxpeax-x86 2.2.0's unconditional
64-bit destination for MOVSXD without REX.W, exposed by Blink's native assembly
regression and covered by a focused test. The adapter also corrects 32-bit BSWAP without
REX.W: the decoder reports a qword register, which caused musl strstr to return
false matches and tar to strip valid filename prefixes. A unit test and native
compiled-C comparison cover the correction.

AArch64 uses yaxpeax-arm 0.5.0 and yaxpeax-arch 0.3.3 as test references, including
100,000 deterministic encoding probes. This is not a complete coverage audit.
The three yaxpeax packages are 0BSD. Formatting/serde/default features are off.
The x86 decoder and shared reader contain unsafe internals; our adapter supplies
bounded slices to their safe interfaces. The A64 decoder file has no explicit
unsafe blocks, but its dependencies do. Our own crates forbid unsafe code.
The host xattr layer uses pinned xattr 1.6.1 and its Rustix dependencies through
safe standard-file/path interfaces; those dependencies contain unsafe internals.
A full dependency soundness review and SIMD/atomic/system-register coverage
matrix remain before broadening advertised capabilities.

## Reference evidence

Source baseline: `29f3fb08563a3adab59372a3c20debc69bf44671`, the builder's preserved
experimental runtime, including PR #218 changes. Builder checkout:
`02d15f5826de5cbc90895b6c973730eeb297505e`.

The preexisting C executable, config, clean-build log and hash manifest are in
`/workspace/work/blink-baseline`. The clean reference build uses the separate
`o/rust-reference` directory. On this Linux x86_64 host, Rust, C interpreter and
native Linux agree on the original fixtures, filesystem/sendfile, and signal
restart. All 12 fixtures pass Rust/native. C differs on vfork memory sharing and
returns extra F_GETFL flags on a CLOEXEC pipe in the descriptor fixture. C JIT fails the pipe/fork
fixture and crashes during the exec fixture, including after a clean rebuild.
Those failures remain visible; C is a comparison target, not an infallible oracle.
The Alpine OCI layer used for smoke tests is
`sha256:63e7e49699e8f5b93162e14c630db8294f87799bfec4376677160876bd6840dc`.
The smoke report also records the BusyBox executable hash.


## Continuous checks

`.github/workflows/rust.yml` runs workspace tests and Clippy on Ubuntu and macOS.
Linux additionally runs the original assembly subset and Rust/native differential
fixtures. `--rust-native-only` deliberately selects those two engines for CI;
the default differential command still reports every C baseline difference.
The macOS job is a validation gate, not a claim that agent-platform builds pass.

In the current cloud environment, Alpine's package CDN is absent from the
configured network allowlist. Download/install coverage remains blocked there,
in addition to the still-unimplemented guest Internet socket backend.
