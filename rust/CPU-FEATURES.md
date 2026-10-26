# AMD64 feature contract

CPUID is implemented independently of yaxpeax's decoder acceptance. Leaf 1 EDX
advertises FPU (0), CX8 (8), CMOV (15), MMX (23), FXSR (24), SSE (25), and SSE2 (26). Leaf 1 ECX advertises CX16 (13) and the hypervisor indicator (31). Other optional feature bits
remain clear, including SSE3, SSSE3, SSE4, AVX, BMI and extended state.
Individual instructions outside these groups may work without their entire
feature group being advertised. This contract does not claim full Linux ABI,
JIT, debugger, or x87 parity.

| Group | Implemented behavior | Qualification |
| --- | --- | --- |
| FPU | Software 80-bit stack/arithmetic, loads/stores, comparisons, constants, remainder/scale/extract, transcendental approximations, legacy environments, masked/unmasked exception completion and deferred guest SIGFPE | 273,926 exact arithmetic records; 7,200 stack-state records; 32,928 exception records; 597,360 independently bounded transcendental records; native signal recovery and cross-host replay |
| CX8 / CX16 | CMPXCHG8B/16B, locked and unlocked forms, write-access validation on both outcomes, 16-byte alignment for CX16 | Native success/failure and upper-register/flag checks; read-only, page-crossing and alignment fault tests |
| CMOV | All integer conditional moves, FCMOVcc and FCOMI/FUCOMI forms | Original cmov assembly gate, native floating-point comparisons and 7,200 conditional-move/stack-state records |
| MMX | MOVD/Q, packed wrap/saturating arithmetic, products, multiply-add, comparisons, logical operations, shifts, packs/unpacks, EMMS; physical x87 aliases, tags/TOP and pending exception checks | 62 MMX cases in the packed native oracle; x87/FXSAVE transition fixture; memory/fault tests |
| FXSR | FXSAVE/FXRSTOR including REX.W format, x87 tags/registers/control/status, XMM0–15, MXCSR, reserved-bit checks and software tail | Native save/restore fixture, signal return and alignment/state tests |
| SSE integer extensions | Averages, min/max, sign masks, insertion/extraction, word shuffles, unsigned multiply-high, byte differences, MASKMOVQ, MOVNTQ, SFENCE and prefetch hints | Packed native oracle and masked-store tests |
| SSE floating point | Single scalar/packed arithmetic, min/max, comparison predicates, integer conversions, register/memory moves, lane moves/unpacks/shuffles, logical operations, LDMXCSR/STMXCSR, RCP/RSQRT | Full-lane and scalar native floating-point oracle; independent reciprocal error-bound oracle; page/alignment/fault tests |
| SSE2 floating point | Double scalar/packed arithmetic, square roots, min/max, comparisons, all SSE2 float/integer conversions, moves, lane operations and logical operations | Native result/MXCSR records across rounding/DAZ/FTZ modes |
| SSE2 integer extensions | XMM forms of packed integer operations; qword add/subtract, unsigned dword multiplication, byte shifts, dword and high/low word shuffles, qword unpacks, MMX/XMM transfers, MASKMOVDQU, MOVNTDQ/MOVNTI, fences and PAUSE | Packed native oracle, streaming stores, alignment/page-boundary tests |

The floating-point oracle compares 952,000 complete output/MXCSR records,
including both halves of packed results. Packed integer/move tests compare
57,344 complete register/flag records. These deterministic corpora include edge
values and pseudo-random inputs; they are substantial regression evidence, not
exhaustive proofs over all encodings and operands.

RCP/RSQRT are allowed implementation-dependent result bits. The separate 131,072
record gate validates the specified relative-error bound (1.5 * 2^-12), exact
special-value handling, scalar upper-lane preservation, and unchanged MXCSR with
exception masks set and clear. Rust uses software arithmetic with a tighter
error bound. Native and Rust outputs are validated independently.

Linux generates the native AMD64 fixtures and oracle artifacts. CI executes the
same AMD64 binaries under Rust on Darwin arm64, comparing exact outputs for
ordinary SIMD operations and the architectural bound for reciprocal operations.

Guest memory is sequentially interpreted, so fences and non-temporal/cache hints
need no host cache operation. CLFLUSH still checks the addressed guest byte.
The separate CLFSH CPUID bit is not advertised. Masked stores access selected
bytes only; the ISA permits implementations to omit faults for masked-off bytes.

The remaining full-runtime goal includes complete x87, newer vector/integer
extensions, synchronous signal-frame details, Linux syscall coverage and the
builder/OCI end-to-end gates. Passing this feature contract does not close issue #1.

The FPU bit was enabled only after adding the baseline transcendental operations,
legacy state operations, trap-adjusted arithmetic/scale/transcendental results,
load/store completion rules and guest SIGFPE delivery. The decoder alone is not
evidence for a feature bit. Undefined/out-of-range inputs (such as invalid BCD
digits or F2XM1 outside its defined interval) and undocumented opcodes are outside
this qualified contract. Transcendental last bits and the narrowly documented
minimum-normal underflow boundary can differ between physical CPUs and Rust.
The regression bounds, state comparisons and known limits are in README.md;
this feature advertisement is not a claim of exhaustive x87 conformance.

Leaf 0 exposes an Intel-compatible vendor and leaf 1 a generic family-6 signature
because glibc 2.36 does not read basic feature bits for unknown vendors. The
hypervisor bit is set and leaf 0x40000000 identifies `BlinkRustCPU`. This is a
virtual compatibility identity, not a description of the host CPU or a claim
that additional Intel instruction groups are available.
