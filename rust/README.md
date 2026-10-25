# Rust runtime migration

Goal: [issue #1](https://github.com/brandonros/blink/issues/1), with early AArch64
contract validation from [issue #2](https://github.com/brandonros/blink/issues/2).
The C implementation remains intact. This is an experimental, incomplete Rust
interpreter, not builder parity or a replacement for the existing CLI.

## Run and verify

Rust 1.82+ and no third-party Rust dependencies are required for this bootstrap.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p blink-cli -- --root / --arch x86_64 /path/to/static-guest
python3 rust/differential.py --c-blink /path/to/frozen/c-blink
```

The differential harness compiles assembly fixtures with `cc`, writes its report
under `target/differential`, and checks Rust, C interpreter, C JIT, and native
Linux AMD64 when available. Nonzero status reports a mismatch; all results are
retained, including failures in the C baseline.

## Current implementation

The eight crates follow the issue's dependency boundaries. `blink` exposes
`Blink::create`, `run`, `run_with_io`, and `close`. Each run owns its CPU, address
spaces, process table, descriptor copies, pipes, and zombies. The cooperative
scheduler never forks the host, installs signal handlers, or executes guest
instructions natively. Returning or failing drops the entire guest process tree.

Implemented slices:

- Checked, page-backed memory with R/W/X permissions and resource bounds.
- Static little-endian ELF64 ET_EXEC loading, zeroed BSS, argv/env/limited auxv.
- Small AMD64 integer/addressing/branch/stack subset and AArch64 integer,
  address generation, branch, and unsigned-offset load/store subset.
- Linux write/read, fork (AArch64 clone(SIGCHLD)), execve, wait4, pipes,
  close/dup, process IDs, exit, and basic SIGKILL/SIGTERM delivery.
- Bounded output capture, instruction budgets, timeouts, cancellation including
  blocked guest pipes, explicit outcomes, and failure-then-success reuse.

Unsupported opcodes and syscall variants are explicit engine errors. No C
fallback, JIT, decoder-based capability advertisement, or kernel VM is used.

## Limits and next gates

This does not run a shell, package manager, Node, Bun, or either builder image.
Dynamic loading, broad ISA coverage, threads/TLS/futexes, signal handlers and
frames, VFS/rootless metadata, filesystem writes, networking, and JIT remain.
Memory accounting bounds resident mapped guest pages across forked processes;
allocator overhead, ELF input buffers, and host sinks are separate. Guest input
is EOF in this slice. Caller-provided output sinks must return promptly; arbitrary
blocking host `Write` calls cannot yet be interrupted. Rooted canonical paths
are build bookkeeping, not a security sandbox; Linux absolute symlink semantics
and non-UTF8 exec strings are not implemented. ELF mappings sharing pages are
currently rejected. No host compatibility beyond the tested Linux host is claimed.

The small decoders currently implement only the execution subset. Evaluate pinned
yaxpeax adapters before expanding instruction coverage; do not silently accept
instructions beyond implemented semantics. Any translated C sources must preserve
the repository's upstream ISC notices.

## Reference evidence

Source baseline: `29f3fb08563a3adab59372a3c20debc69bf44671`, the builder's preserved
experimental runtime, including PR #218 changes. Builder checkout:
`02d15f5826de5cbc90895b6c973730eeb297505e`.

The cloud's preexisting C executable and config are preserved in
`/workspace/work/blink-baseline`. A clean C build uses the separate
`o/rust-reference` directory. On this Linux x86_64 host, the clean C interpreter
and native control pass all four initial differential fixtures; C JIT fails the
pipe/fork fixture and crashes during the exec fixture. Rust passes all four.
These baseline failures are recorded rather than treating C as an infallible oracle.
