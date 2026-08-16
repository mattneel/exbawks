# ADR 0009: Non-Leaf Generated Block ABI

- Status: accepted
- Date: 2026-08-15

## Context

ADR 0006 fixes generated blocks as leaf functions with `RAX` and `RDX` as the
only scratch registers. Memory-operand translation (`JIT-004`) must call
checked guest-memory helpers, and later stages call CPUID/RDTSC and string
helpers, so blocks stop being leaves. The codegen contract requires a new ADR
before material changes.

## Decision

Generated blocks keep the ADR 0006 entry signature
`extern "C" fn(*mut CpuState) -> u64` but become non-leaf functions.

The prologue reserves one fixed frame: 32 bytes of Windows x64 shadow space
plus alignment padding so `RSP` is 16-byte aligned at every helper call site.
The epilogue releases the same fixed amount. Frame size is a single constant
per backend; blocks never allocate dynamically.

`RCX` still carries the `CpuState` pointer for the block lifetime. Because
helper calls clobber volatile registers, the emitter saves `RCX` in `RSI` or
`RBX` (a callee-saved register pushed in the prologue) and restores guest-state
addressing from it after every call.

Helper signatures follow the Windows x64 convention and take the raw context
the runtime installed for the block's execution, for example:
`extern "C" fn(memory: *const MemoryContext, address: u32, output: *mut u32) -> u32`
returning a status discriminant. Helpers never unwind into generated code;
they translate every failure into a status the block routes to a structured
exit (`MemorySlowPath` carrying the fault details in `CpuState`-adjacent
scratch fields defined by the codegen contract).

Before any helper call the block materializes all live guest state into
`CpuState` (ADR 0006 already spills per instruction, so this holds trivially
for the direct backend). Guest EFLAGS are materialized before the call and
reloaded after, because helpers may execute flag-changing host code.

Host segment state is never modified by generated code; helpers therefore run
with intact host `FS`/`GS` per the repository invariant.

## Consequences

The direct backend gains one prologue/epilogue pair per block; register-only
blocks pay a few bytes of overhead for uniformity, which keeps the dispatcher
unchanged.

Every helper boundary is a controlled point where guest state is complete in
`CpuState`, which fault redirection (ADR 0004) and trace tooling rely on.

A future register-caching backend must re-specify materialization points in a
new ADR, as ADR 0006 already requires.

`docs/codegen-contract.md` gains the frame constant, the helper status
encoding, and the `RCX` preservation rule; `JIT-004` lands against that
updated contract.
