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
  and report missing/non-socket targets. Connected AF_UNIX stream socket pairs
  support read/write, readv/writev, send/recv, sendfile, half-close, poll, peek,
  nonblocking flags shared across duplicates, and close-on-exec. Their bounded
  queues and endpoint lifetimes are guest-owned; no host socket is opened.
  Pathname bind/listen/accept and ancillary data remain. Opt-in TCP support is
  described below; UDP and guest DNS remain.
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
CPUID capability set. `apk-smoke.py` passes seven offline gates: version, installed-package listing,
simulation, a persisted world update, installation of a newly constructed local
APKv2 package, execution/metadata verification, and removal (including BusyBox's
trigger). The package contains payload checksums and a post-install shell script;
UID/GID 123:456, mode 0640, and mtime 1 survive into a subsequent guest invocation
without changing host ownership. The unsigned fixture explicitly uses
`--allow-untrusted --force-non-repository` in a disposable root without persistent
APK cache. No repository package download or signature-trust gate is claimed.
Node, Bun, and builder images are unproven.

The six small builder examples `00-hello` through `05-environment` pass every
RUN and CMD via `builder-smoke.py`, including gzip/tar round trip and SHA-256.
The `07-proc` example also passes against the virtual guest proc filesystem.
This harness reads their original Dockerfiles, retains a disposable root between
steps, and checks expected output. It does not assemble/export an OCI image or
replace the 26 agent-platform checkpoint receipts.
Broad ISA/SIMD/FPU coverage, shared file mappings, remaining directory-relative
*at operations, broader VFS/procfs, UDP/DNS, and JIT remain. ARM64 signal frames,
threaded exec, queued realtime signals, job control,
handlers for synchronous faults remain.
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
GitHub Actions at `9d080d5f06a094cb2af297db88c880ed74a3a78e` passed workspace
tests, formatting, and Clippy on Darwin arm64 and Linux AMD64, plus the assembly
and 13 compiled native comparisons on Linux. Full builder workloads have not yet
been validated on macOS. Directory enumeration tests do not assume host ordering;
signal restart tests acknowledge progress through a pipe rather than guessing a
host-speed delay.

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
native Linux comparisons retain per-fixture results. The expanded filesystem,
MXCSR restart, socketpair and alternate-stack cases expose additional C baseline
differences; consult the report rather than treating the C runtime as the oracle. All 17 fixtures pass Rust/native. C differs on vfork memory sharing and
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
Both hosts also prepare the pinned AMD64 Alpine fixture with
`python3 rust/alpine-fixture.py --root target/amd64-alpine` and run dynamic musl,
seven builder examples, rootless metadata persistence, and seven offline APK
checks. The fixture loader verifies OCI digests and needs no container engine;
`--layout` reuses an already downloaded OCI layout. macOS copies preserve the
metadata xattrs using `ditto`. These are validation gates, not a claim that all
agent-platform checkpoints or OCI export verification pass.

In the current cloud environment, Alpine's package CDN is absent from the
configured network allowlist. Download/install coverage remains blocked there,
independently of the opt-in guest TCP backend. A newly published environment
configuration has not yet appeared in this running instance's active policy.

## Floating-point interpreter

SSE scalar and packed single/double add, subtract, multiply, divide, square root, minimum,
maximum, comparisons, scalar and packed integer conversions, and single/double conversions
use pinned `rustc_apfloat` 0.2.3+llvm-462a31f5a5ab (Apache-2.0 WITH LLVM-exception).
Arithmetic is software Rust, with no host floating-point mode changes or C
fallback. MXCSR rounding, DAZ/FTZ, sticky exception bits, masked results, and
unmasked arithmetic faults are explicit guest state. Signal frames save/restore
MXCSR and handlers start with the default control word. Synchronous fault
handlers remain unsupported; an unmasked SIMD fault terminates the guest.

`python3 rust/float-regressions.py` compares result bits and MXCSR against native
Linux AMD64 for 435,200 scalar and packed records, covering exceptional values, four rounding
modes, DAZ/FTZ combinations, and deterministic random inputs. Unit tests cover
packed exceptions and page-end scalar reads. The adapter accounts for x86 NaN
priority and directed-overflow flags: APFloat's saturation-to-maximum path reports
only INEXACT, so a same-precision widened-exponent calculation detects overflow.
These checks do not establish complete SSE2, x87, transcendental, or FPU coverage.
CPUID remains conservative. CI also replays the native-generated AMD64 fixture
and golden records on Apple Silicon; its status must be checked before claiming
that cross-host comparison passed.

Square root uses a bounded integer restoring algorithm on at most 106 significand
bits, including exact remainder-based rounding. No additional square-root
dependency is linked. A further 100,000 deterministic single/double pairs compare
nearest rounding with the host square-root operation in tests.

APKv2 installation also exercises directory-relative ownership/timestamp updates
and fchdir. `chroot` supports reselecting the existing configured root (as used by
APK install scripts); changing to another guest root remains unsupported. Open
directory handles still carry a stored path, so rename/unlink semantics remain
incomplete. No host chroot is performed.

## TCP and clocks

`RuntimeConfig.network = true` / CLI `--network` permits guest IPv4/IPv6 TCP
connections through safe Rustix host handles. All host sockets remain nonblocking;
blocking guest connect/read/write waits replay through the cooperative scheduler.
Descriptor duplication/fork share the socket and status flags, close-on-exec is
per descriptor, and all handles close with their last owner. Supported operations
include connect, read/write/readv/writev, send/recv, poll, shutdown, socket names,
SO_ERROR, TCP_NODELAY and SO_KEEPALIVE. Unsupported socket options fail explicitly.
UDP, bind/listen/accept, sendmsg/recvmsg, ancillary data, and guest DNS remain.
Network permission does not bypass host routing or environment network policy.

`tcp-smoke.py` compares a loopback client natively and under Rust, including
blocking/nonblocking connects, split responses, half-close, fork/descriptor
lifetime, and a runtime timeout while waiting for the host peer. A real Alpine
BusyBox wget also downloaded Debian's 1,198-byte README over HTTP through this
cloud environment's configured proxy. HTTPS and remote APK downloads are not
established by that HTTP check. Guest proxy variables must be supplied explicitly;
the runtime does not automatically inherit host environment or credentials.

Guest real interval timers/alarm generate guest SIGALRM, are cleared in forked
children, and remain through exec. Threads share their process timer; pending
standard signals coalesce. Wall/monotonic clock queries use host clock adapters.
Nanosleep and absolute/relative clock_nanosleep cooperate with scheduling and
write remaining time on interruption. Virtual/profiling CPU timers and other
clock IDs remain unsupported. The native comparison suite now has 17 compiled
fixtures, including real-timer delivery, fork inheritance, and interrupted sleep.

Packed conversion memory operands are checked at their architectural width: CVTDQ2PD and CVTPS2PD read eight bytes, even though the decoder reports a 16-byte vector width. Page-boundary and unmasked-exception tests guard source accesses and whole-destination preservation.

AMD64 alternate signal stacks support sigaltstack, SA_ONSTACK, nested handlers,
and SS_AUTODISARM. The guest ucontext saves/restores the stack configuration;
fork inherits it, thread clone disables it, and exec resets its address/size.
The native comparison checks stack placement, mutation rejection while active,
autodisarm/restoration and fork/exec inheritance. No host signal stack changes.
The current Alpine workload and 435,200-record floating-point gates passed on
both Linux AMD64 and Darwin arm64 at commit 8cde89c.

Guest SIGCHLD now reports child PID, UID, CLD_EXITED/CLD_KILLED and exit status.
Explicit SIG_IGN and SA_NOCLDWAIT reap children automatically; the default
SIGCHLD disposition still leaves a waitable zombie. Standard pending signals
retain the first event's metadata while coalescing. rt_sigpending, pause and
rt_sigsuspend are supported; sigsuspend restores the original mask on handler
return and returns EINTR even with SA_RESTART. The native fixture checks these
behaviors together with child-exit notifications. Child resource-usage accounting,
stop/continue notifications, and queued realtime delivery remain unsupported.

## Initial x87 interpreter

The guest owns an eight-entry 80-bit register stack, tags, control/status words,
and deferred arithmetic exceptions. Software APFloat handles extended-precision
add/subtract/multiply/divide, comparisons, integral rounding, integer and float
loads/stores, and stack operations. Signal handlers start with a fresh x87 state;
sigreturn restores the saved stack and control word. An unmasked exception
preserves the destination/stack and faults on the next waiting instruction.

`python3 rust/x87-regressions.py` compares 96,948 native AMD64 result/status records,
including 80-bit extrema, NaNs, denormals, all three precision controls, four rounding modes, comparisons and
conversions. Tests also cover a ten-byte source at a page boundary, masked stack
overflow, deferred faults, and signal-frame preservation. The adapter handles
x87 NaN priority and the APFloat directed-overflow flag difference. CI replays
this native-generated fixture on Apple Silicon as well as the SSE fixture.

This is partial x87 support. Transcendentals, square root,
legacy environment/save-state instructions and complete
unsupported-encoding behavior remain; unsupported operations fail explicitly.
CPUID still does not advertise complete FPU/SSE2 support. No Node/Bun workload or
new agent-platform checkpoint is established by these instruction tests.

The 24/53-bit x87 arithmetic modes use a 111-bit intermediate with a wider
exponent range. Discarded bits are retained through round-to-odd before final
rounding, avoiding double rounding at a midpoint plus a very small term. Native
fixtures include those cases and rounding-dependent signed zero after exact
cancellation. The 64-bit precision path retains direct extended arithmetic.

FXSAVE/FXRSTOR support the legacy and REX.W formats, x87 stack/tag/control state,
all 16 XMM registers, MXCSR validation, 16-byte alignment faults and preservation
of the software-owned tail. The native fixture checks restoration and fault
behavior. The runtime records the last x87 instruction/opcode/data address;
processor-specific pointer reporting is not compared byte-for-byte by this gate.
