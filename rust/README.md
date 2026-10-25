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

CPUID exposes the qualified CMOV/MMX/FXSR/SSE/SSE2 groups described in [CPU-FEATURES.md](CPU-FEATURES.md). Unsupported opcodes,
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
Pinned Alpine Node gates are described below. Bun and complete builder images remain unproven.

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
differences; consult the report rather than treating the C runtime as the oracle. All 25 fixtures pass Rust/native. C differs on vfork memory sharing and
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
Linux AMD64 for 952,000 scalar and packed records, covering exceptional values, four rounding
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
clock IDs remain unsupported. The native comparison suite now has 25 compiled
fixtures, including real-timer delivery, fork inheritance, and interrupted sleep.

Packed conversion memory operands are checked at their architectural width: CVTDQ2PD and CVTPS2PD read eight bytes, even though the decoder reports a 16-byte vector width. Page-boundary and unmasked-exception tests guard source accesses and whole-destination preservation.

AMD64 alternate signal stacks support sigaltstack, SA_ONSTACK, nested handlers,
and SS_AUTODISARM. The guest ucontext saves/restores the stack configuration;
fork inherits it, thread clone disables it, and exec resets its address/size.
The native comparison checks stack placement, mutation rejection while active,
autodisarm/restoration and fork/exec inheritance. No host signal stack changes.
The current Alpine workload and 952,000-record floating-point gates passed on
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

`python3 rust/x87-regressions.py` compares 273,926 native AMD64 result/status records,
including 80-bit extrema, NaNs, denormals, all three precision controls, four rounding modes, comparisons and
conversions. Tests also cover a ten-byte source at a page boundary, masked stack
overflow, deferred faults, and signal-frame preservation. The adapter handles
x87 NaN priority and the APFloat directed-overflow flag difference. CI replays
this native-generated fixture on Apple Silicon as well as the SSE fixture.

This is partial x87 support. Transcendentals, remaining arithmetic operations and complete
unsupported-encoding behavior remain; unsupported operations fail explicitly.
The complete x87 FPU feature remains unadvertised. No Node/Bun workload or
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

## Node startup gate

The pinned AMD64 Node 22.23.4 Alpine image prints its version and exits under the
Rust interpreter. `node-fixture.py` extracts only that digest from a layout pulled
by linux-oci-builder; `node-smoke.py --root ROOT` runs version, arithmetic and filesystem gates. The image
manifest is `sha256:025b8e975a61d94e33abeafaad1255ef44e4898cf1a1b6446860009c72e6d284`;
the Node executable SHA-256 is `5a92a46a1d8279fec6776e3123ca22e26a6fddbb7cf1b55e019dbb38eea8ff26`.
The JavaScript gate uses `--jitless`: the current `--jitless -p '1 + 2'`
probe has progressed through thread startup and SIMD/conditional-move feature checks,
and now evaluates JavaScript, runs the builder's file-I/O body and exits cleanly
with `--jitless`. Normal Node JavaScript evaluation and file I/O also exit cleanly after
separating virtual reservations from backed-memory accounting for its 512 MiB CodeRange.
The feature qualifications are documented in [CPU-FEATURES.md](CPU-FEATURES.md). CLI `--max-memory BYTES` and `--max-image-size BYTES`
expose existing runtime limits for this 130 MB executable.

Startup exercises word/dword packed shifts, saturating packing, lane shuffles,
and packed logical operations. `packed-regressions.py` compares 57,344 complete
native register/flag records; CI also replays them on Apple Silicon. Guest capget
supports version negotiation and guest PID queries. Its read-only model derives
effective/permitted capabilities from guest root IDs; it never exposes host
capabilities. Capset, securebits and file capabilities remain unsupported.
Coarse realtime/monotonic clocks use one-millisecond guest resolution. Signal
action flags are read as 32 bits, accepting musl's sign extension of high flags.

The same pinned Node version check passes natively and with the preserved C
interpreter when invoked through the image's musl loader and explicit library
path. The Linux C reference was built with DISABLE_VFS, so BLINK_PREFIX does not
root that binary; those controls are not equivalent to the Rust rooted workload.
The C configuration/artifacts remain preserved rather than silently rebuilt with
different features for this comparison.


## Guest event loops

Guest-owned eventfd counters and epoll support level readiness, one-shot rearming,
fair delivery, dup/fork sharing, and watch removal after the last open description
closes. Watches do not retain their target descriptors. epoll_pwait restores the
caller's signal mask on return or interruption, including with SA_RESTART.
Nonblocking pipe endpoints share their status flags across dup/fork and implement
EAGAIN for empty reads/full writes, vectored I/O, and bounded atomic writes.

The epoll native fixture checks these lifetimes, descriptor reuse, semaphore and
overflow counters, bad output pointers, temporary signal masks and nonblocking
pipes. Edge-triggered/exclusive/nested epoll remain explicitly unsupported.
Optional io_uring and membarrier return ENOSYS, allowing guest libraries to use
their fallback paths. Obsolete CLONE_DETACHED is ignored as on Linux.


Guest PR_SET_NAME/PR_GET_NAME preserve per-thread names across fork, reset them on
exec, truncate to 15 bytes and handle guest page boundaries without host prctl.
The native suite checks these semantics and TZCNT/LZCNT results plus defined
CF/ZF flags at all operand widths. Lifecycle tests cancel blocked epoll, eventfd
and pipe waits, then reuse the same runtime. The pinned Node version gate has
passed on Linux AMD64 and Darwin arm64 CI; The later requeue gate establishes `--jitless` JavaScript and file I/O with clean exit.


Packed SIMD coverage includes multiply-high/low, unsigned dword multiplication,
signed multiply-add with wrapping overflow, byte absolute-difference sums,
rounded averages, saturated addition/subtraction, word min/max, lane insertion
and extraction, sign-bit masks, unpacking and non-temporal stores. The 57,344
native records compare full result registers and preserved flags. Memory tests
cover aligned-only operand faults and a two-byte PINSRW at a page boundary.
The later MMX, reciprocal and memory gates complete the baseline SIMD qualification; see [CPU-FEATURES.md](CPU-FEATURES.md).


MMX registers alias the physical guest x87 register storage. Successful MMX
instructions enter the packed register state, EMMS empties its tags, and pending
unmasked x87 exceptions still fault before MMX executes. Native FXSAVE comparisons
check tag/TOP transitions and the 80-bit register aliases. The packed suite now
includes 62 MMX cases; six MMX/float conversions extend the floating-point suite.
Page-end conversion and fault tests check access widths and unchanged state.
Feature advertisement follows the documented instruction groups and native gates.


RCPSS/RCPPS and RSQRTSS/RSQRTPS use host-independent software arithmetic.
Their approximation bits need not match a particular processor: the separate
131,072-record gate checks the architectural relative-error bound, special-value
bits, preserved scalar lanes, and unchanged MXCSR with masks both set and clear.
Both native and Rust outputs are validated, and CI replays that validation on
Apple Silicon. MASKMOVQ/MASKMOVDQU write only selected guest bytes and honor
address-size and FS addressing; masked-off bytes do not require mapped memory.


The floating-point oracle now includes full packed arithmetic, square roots and
all comparison predicates, not only scalar operations and conversions.
CLFLUSH checks the addressed guest byte while requiring no host cache operation;
PAUSE and memory fences preserve state under the sequential guest interpreter.
Native tests retain the distinction between unaligned MOVNTQ (accepted) and
aligned-only XMM streaming stores.


FCMOVcc and x87 empty-stack behavior have a separate 6,048-record native gate.
It includes both taken and untaken conditional moves, signaling/quiet NaNs and
empty source/destination slots. Empty-stack arithmetic and sign operations
produce the negative indefinite value even when another NaN is available.
CMOV advertisement covers the integer and x87 forms together.


MADV_DONTFORK/MADV_DOFORK apply per guest page, independent of read/write
permissions. Fork copies only inherited mappings and accounts for those copied
bytes; threads and vfork retain their shared address space. The native fixture
checks partial-page rounding, PROT_NONE, inheritance restoration and parent/child
isolation. Guest sysinfo reports runtime memory limits/free budget, task count
and elapsed run time, with no guest swap or host memory statistics.


Private anonymous mremap supports in-place resize, MAYMOVE and FIXED replacement,
preserving data, protections and fork inheritance. New pages are zero-filled,
shrunk ranges are unmapped, and aggregate runtime limits remain enforced.
File-backed mappings can move/shrink, but extending them remains unsupported;
DONTUNMAP, shared mappings and full Linux VMA split/merge behavior are not covered.
The native remap fixture checks blocked growth, moves, replacement, invalid ranges,
read-only pages, fork advice and shrink behavior.


Guest standard streams share nonblocking status across dup/fork without changing
host fd flags. WouldBlock from an output callback becomes guest EAGAIN for a
nonblocking descriptor or a cancellable scheduler wait otherwise; partial writes
retain their counts. Individual transfers are bounded to 64 KiB. A host Write
callback that blocks inside its implementation still cannot be interrupted.
The page-based decoder and bulk-copy path retain permission/page-boundary checks
and self-modifying code behavior; the pinned Node version probe dropped from
about 16 seconds to 4 seconds on this development host.


FUTEX_REQUEUE and FUTEX_CMP_REQUEUE move actual blocked guest waiters without
waking transferred threads; failed comparisons leave queues unchanged. The native
three-thread test checks wake/transfer counts and subsequent wakes on each address.
Node smoke now requires version, JavaScript evaluation, and the existing builder
checkpoint's file-I/O body to exit zero. The latter two use --jitless and the pinned
Alpine/musl image, so they are not the original Debian-image checkpoint or a JIT
claim. The arithmetic control also passes natively and in the preserved C
interpreter with an explicit musl loader; the C control is not rootfs-isolated.


## Virtual reservations and page discard

`max_memory` / `--max-memory` limits storage-backed guest pages (64 MiB by
default). `max_virtual_memory` / `--max-virtual-memory` separately limits mapped
address space (4 GiB by default). Both budgets apply across the guest process
tree, counting shared thread/vfork address spaces once. Inaccessible anonymous
reservations allocate page metadata without page contents. Enabling access
allocates zeroed backing; disabling access preserves previous contents and their
budget. Metadata and runtime overhead are additional host allocations, bounded
by the virtual-page and task limits rather than included in `max_memory`.

Anonymous private `MADV_DONTNEED` zeroes accessible pages and releases backing
for inaccessible pages. It preserves permissions and fork advice. Discarding
file-backed contents remains unsupported because reload provenance is not yet
retained. Native comparisons cover a 512 MiB reservation, protection changes,
content preservation and discard. A separate runtime policy test checks that
fork cannot duplicate reservations beyond the aggregate virtual budget.

The five pinned Node cases pass locally: version, arithmetic and file I/O with
and without `--jitless`. Normal arithmetic also agrees with native Linux and
the preserved C interpreter. A separate 100,000-iteration JavaScript loop prints
`14999950000` and exits cleanly; this does not establish complete V8 coverage
or implement a Rust host JIT.

Double-width CMPXCHG8B/16B is implemented as one cooperative interpreter step,
with write-access checks on success and failure, architectural register widths,
ZF-only updates and CX16 alignment checks. CPUID advertises CX8/CX16 after
native comparisons and fault-state tests; CI replays the fixture on Darwin arm64.

FSQRT uses a 128-bit integer radicand and exact remainder, rounding directly
to the selected 24/53/64-bit significand. The x87 oracle now compares 273,926
records, including 49,248 extra square-root cases with extended-format edge
encodings, exponent extremes, random significands and all rounding/precision
controls. Empty-stack and deferred unmasked-invalid behavior have focused tests.

The x87 constant loads honor rounding control without precision exceptions or
precision-control truncation. FXAM classifies empty, zero, normal, denormal,
infinite, NaN and unsupported encodings without arithmetic exceptions. Native
comparisons cover all constant rounding/precision combinations and classification
of occupied and empty slots.

Legacy FNSTENV/FLDENV and FNSAVE/FRSTOR support the 14/28-byte environment
and 94/108-byte full images, operand-size overrides, physical register tags,
logical stack ordering and exception masks. Save/reset and load round trips match
native Linux for both formats; CI replays that fixture on Apple Silicon.
Cross-page failures preserve the entire FPU state and destination bytes.
FNINIT and FNSAVE reset control/tag state while preserving register contents.

FSCALE scales extended values with truncation of ST(1), rounding of the result
and overflow/underflow flags, without narrowing to the precision-control width.
FXTRACT produces both the signed significand and exponent, including zeros,
infinities, NaNs and denormals. Native tests cover both outputs and both stack
overflow destinations; unmasked overflow leaves both destinations unchanged.

FPREM/FPREM1 implement truncating and nearest-even remainder with quotient
condition codes. Large exponent gaps use the ISA-permitted 32-bit partial
reduction and set C2 until complete; intermediate reductions need not match a
particular physical CPU. Native comparisons loop to completion and compare
final remainder bits, quotient flags, denormals, NaNs and invalid operations.

FBLD/FBSTP support valid 18-digit packed decimal, signed zero, rounding modes,
precision/invalid exceptions and the indefinite result for invalid stores.
Malformed input digit nibbles remain explicitly unsupported. Native comparisons
include 2,048 decimal loads and extended-value stores across rounding controls.

## Stage worker protocol

`cargo build --release -p blink-cli` also builds `blink-worker`. It accepts one
JSON request per line on stdin and emits one JSON reply per line on stdout.
Guest stdin is EOF; guest stdout/stderr go to distinct, newly created host files.
This is a trusted host adapter for builders, not a guest-facing RPC service.
The worker has no downloader, Dockerfile parser or OCI assembly logic.

Create a context with `{"op":"create","context":"stage","rootfs":"/path/to/root","rootless_metadata":true}`.
Run commands with `{"op":"run","context":"stage","argv":["/bin/sh","-c","echo hello"],"timeout_ms":10000,"stdout":"/path/to/new.stdout","stderr":"/path/to/new.stderr"}`.
Run requests optionally specify `cwd`, `env` (a string map), `uid` and `gid`.
Close explicitly with `{"op":"close","context":"stage"}`; EOF closes remaining
contexts. Errors return `ok:false`; completed executions return `ok:true` plus
`termination`, `code` and `instructions`. A nonzero exit is an execution outcome,
not an engine error. Requests are sequential, capped at 1 MiB, with at most 64
contexts. Use command timeouts; this adapter does not multiplex cancellation
requests while a command runs. The direct library API retains cancellation tokens.

Create defaults are 512 MiB backing, 4 GiB virtual mappings, 256 MiB ELF size,
and one billion interpreted instructions per run, configurable through
`max_memory`, `max_virtual_memory`, `max_image_size`, `max_instructions`.
Host networking is opt-in with `network:true`. Serde 1.0.229 and serde_json
1.0.154 (MIT/Apache-2.0) are pinned for this host protocol, independently of the
architecture decoders' disabled serde features. Cargo.lock pins transitive crates.

`worker-smoke.py` checks persistent filesystem/metadata, isolated contexts,
nonzero-exit and timeout recovery, per-run credentials, protocol isolation and
explicit close. It runs on Linux and Apple Silicon CI.

Integer-memory FICOM/FICOMP support signed 16/32-bit operands and optional
stack pops. The expanded x87 oracle includes pseudo-denormals and unsupported
80-bit encodings across arithmetic, comparisons and stores. Unsupported arithmetic
encodings produce indefinite results with invalid-operation status; pseudo-denormals
raise denormal status and normalize where the operation produces a new value.
Focused boundary cases check tininess before rounding into minimum normal.


FPATAN uses software arithmetic with 126-bit intermediate significands and
quadrant reduction; it never calls the host floating-point environment. Its
approximate result can differ from a physical CPU in the last bit. The separate
`transcend-regressions.py` gate checks 597,360 records against native state and an
independent 200-bit mpmath 1.3.0 reference. The explicit regression bounds are one
extended ulp for nearest rounding and 1.5 ulps for directed rounding. C1 describes
each implementation's internal rounding and is excluded from native bit equality;
other status bits, stack effects and special results must agree, except the
explicit minimum-normal FPATAN underflow boundary described below. Observed maxima
were about 1.265 ulps native and 1 ulp Rust; 22,710 results differed in their last bit.
CI replays the fixture and accuracy checks on Apple Silicon. The exact arithmetic
oracle remains separate and bit-exact. These corpora are not exhaustive proofs,
and the complete x87 FPU feature remains unadvertised.

The persistent builder adapter is in
[linux-oci-builder PR #1](https://github.com/brandonros/linux-oci-builder/pull/1).
Its Linux and Apple Silicon CI verifies context reuse, metadata, timeout/failure
recovery, receipts/resume, root locking and standalone non-root commands. Complete
agent-platform builds and OCI verification remain outstanding.

The transcendental gate also covers F2XM1, FYL2X and FYL2XP1. Exponential
inputs are within [-1, 1] plus special encodings; finite F2XM1 inputs outside
its defined range fail explicitly. FYL2X spans the extended exponent range.
FYL2XP1 numerical qualification uses |x| < 1/4, plus zero/NaN/infinity states,
within its documented accurate input range. Logarithm zero/divide-by-zero and
invalid-operation priority are checked independently of approximation error.

FSIN, FCOS, FSINCOS and FPTAN use the same software intermediates, quadrant
reduction and Taylor series. The oracle checks both stack outputs, signed zeros,
NaNs, and C2/no-push rejection at |x| >= 2^63. Within |x| <= pi/4, the numerical
bounds above apply. Outside that interval, the independent gate bounds sine/cosine
absolute error by 2^-62 * max(1, |x|), and tangent angular residual modulo pi by
the same amount. This explicitly allows physical CPUs' limited pi reduction;
it is not an ulp-accuracy claim for large angles or values near zeros/poles.

At FPATAN(minimum-normal / 1) and FSIN/FSINCOS(minimum-normal), local Intel
hardware and CI native hardware disagree
on underflow status because the mathematical correction is far below extended
precision. The gate permits only UE differences for those exact input boundaries
(and its signed/pseudo-denormal forms), retaining every occurrence in the report.
All other exception comparisons remain strict. C1 and last-bit differences remain
expected for approximate instructions, and full FPU support is still incomplete.

Unmasked precision exceptions now commit the rounded result, memory store and
stack pop before setting the deferred exception state. A separate 32,928-record
native oracle covers add/multiply/divide, square root, rounding, float/integer/BCD
stores and popping arithmetic across precision and rounding controls, with precision, overflow, underflow and denormal exceptions
masked and unmasked. CI replays those exact bytes on Apple Silicon. Basic arithmetic now commits the exponent-adjusted register result on unmasked
overflow/underflow, including popping operations, and gives unmasked denormal
operands priority over subsequent result exceptions. Qualification includes
random significands, signs and near-boundary values across precision controls.
FSCALE also applies trap exponent adjustment, including extreme exponents whose
adjusted result still saturates to infinity/zero. The transcendental gate also covers unmasked overflow/underflow, undoing the
trap exponent adjustment before independent accuracy checks and retaining full
significand precision for adjusted subnormals. Both FSINCOS/FPTAN outputs and
FPATAN/logarithm pops are checked. Synchronous arithmetic faults now enter guest SIGFPE handlers as described
below; these tests still do not establish complete x87 conformance.

Synchronous integer divide, x87 deferred exceptions and SSE exceptions enter
installed AMD64 guest SIGFPE handlers with the faulting instruction address,
Linux si_code and trap number. The rt_sigframe preserves the actual FP state;
the handler starts with clean FP controls and can edit its saved context before
rt_sigreturn. Blocked, ignored or default SIGFPE dispositions terminate the guest.
The native fpsignal fixture checks recovery on an alternate stack for integer
divide, x87 divide/precision and SSE divide faults, plus fatal dispositions in
forked children. It is replayed on Apple Silicon. This does not yet add catchable
SIGSEGV/SIGBUS/SIGILL, core dumps or complete Linux synchronous-signal behavior.

The exception corpus additionally checks unmasked denormal FLD (which normalizes
and pushes before trapping), overflow/underflow FST memory operations (which do
not round, store or pop), signaling-NaN loads/stores and legacy 8087/287 no-ops
with pending exceptions. These native checks caught distinct completion rules
that cannot be represented by a single blanket “unmasked means no write” rule.
