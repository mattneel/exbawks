# ADR 0006: First Executable Block ABI

- Status: accepted
- Date: 2026-08-15

## Context

The codegen contract left the entry register open until `JIT-002`.

Executable register-only blocks land with `JIT-002` and need one fixed entry,
exit, flags, and dispatch contract.

Entering generated code requires unsafe code outside `exbawks-platform`.

## Decision

Generated blocks use the entry signature `extern "C" fn(*mut CpuState) -> u64`
on x86-64 Windows.

The `CpuState` pointer arrives in `RCX` and stays there for the block lifetime.

First-subset blocks are leaf functions. `RAX` and `RDX` are the only scratch
registers. Guest registers spill through the `CpuState` pointer for each
instruction.

Guest arithmetic flags materialize into `CpuState.eflags` after each
flag-writing instruction. `ADD` and `SUB` merge carry, parity, auxiliary
carry, zero, sign, and overflow. `AND`, `OR`, and `XOR` merge the same set
without auxiliary carry, which the hardware leaves undefined, so the guest
value stays unchanged.

The epilogue writes the successor `EIP` into `CpuState` and returns one exit
code in `RAX`. The low 32 bits select the exit kind in contract order: direct,
conditional, indirect, kernel call, memory slow path, unsupported instruction,
budget exhaustion. The high 32 bits stay zero.

The only unsafe code outside `exbawks-platform` lives in
`exbawks-jit/src/dispatch.rs`, which enters sealed code buffers.

## Consequences

Per-instruction spills keep the first backend simple and slow.

A register-caching backend needs a new ADR.

Every future backend and the dispatcher share one exit encoding.

The `exbawks-jit` crate moves from `forbid(unsafe_code)` to `deny` with one
allowed module.
