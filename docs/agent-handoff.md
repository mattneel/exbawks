# Coding Agent Handoff

## Mission

Build Exbawks into a Windows 11 original Xbox research emulator.

Keep the first runtime narrow. Execute one generated synthetic XBE through one kernel HLE call.

## Repository state

The repository is an implementation scaffold with functional pure-logic components.

Implemented components include:

- Checked XBE header and section parsing.
- Retail and debug entry-point key decoding.
- A software guest address space with aliases and permissions.
- A one-million-entry sidecar page table.
- Physical-page generation tracking.
- An `iced-x86` basic-block decoder.
- Direct-rewrite translation plans.
- Code-cache dependency checks.
- Kernel and graphics HLE interfaces.
- Windows section and placeholder ownership wrappers.
- Host memory geometry queries (`MEM-001`).
- Single-owner placeholder split and coalesce operations (`MEM-002`).
- A reserved 4 GiB-aligned high guest arena (`MEM-003`).
- Coherent Windows RAM aliases through pagefile section views (`MEM-004`).
- A Windows `GuestMemory` backend with generated equivalence tests against
  the software backend (`MEM-005`).
- Single-owner executable code buffers with write and execute phases
  (`JIT-001`).
- Register-only direct emission and a dispatcher under the ADR 0006 block
  ABI, verified against an interpreter oracle (`JIT-002`).
- Sorted source ranges and fault-site metadata with binary-search lookups
  (`JIT-003`).
- Kernel thunk gate patching and runtime gate-call dispatch (`HLE-001`).
- The startup kernel export set, a guest stack, and the execution loop
  (`HLE-002`).
- A CLI for host checks, XBE inspection, decoding, thunk inspection, entry
  planning, and synthetic boot execution.
- Generated public test data that boots to a controlled guest exit.

Incomplete components include:

- Memory operand and guest control-flow translation.
- Fault-site redirection and MMIO dispatch.
- The Windows backend as the active runtime address space.
- Extended XBE metadata parsing (certificates, TLS, libraries).
- Graphics decoding and rendering.

Read [the validation report](../VALIDATION.md) before code changes.

## Mandatory reading order

1. Read `AGENTS.md`.
2. Read `docs/architecture.md`.
3. Read `docs/memory.md`.
4. Read `docs/codegen-contract.md`.
5. Read the accepted ADRs in `docs/adr`.
6. Select one task from `docs/task-board.md`.

## Immediate objective

Tasks `MEM-001` through `MEM-005`, `JIT-001` through `JIT-003`, `HLE-001`,
and `HLE-002` are complete. The first execution milestone passes on
Windows 11 x86-64.

Select the next task from the board: `XBE-001`, `DBG-001`, or `QA-001`.
Memory operand rewriting and fault-site redirection follow per the
`AGENTS.md` task sequence.

Do not start graphics work before memory operands translate.

## First commands

```powershell
.\scripts\bootstrap.ps1
python scripts/static-validate.py
cargo xtask check
cargo exbawks doctor
cargo exbawks plan .\fixtures\synthetic\minimal-retail.xbe --json
```

`Cargo.lock` is committed. Keep it current with dependency changes.

## Engineering constraints

Keep guest addresses as typed values.

Keep HLE code independent from low identity mappings.

Track compiled code by physical page.

Keep Windows handles and views under single-owner RAII types.

Keep executable memory non-writable outside emission.

Keep complex work outside exception handlers.

Do not add proprietary Xbox data.

Do not treat malformed guest input as a Rust panic.

## Pull request contract

Use one task identifier in each pull request title.

Add tests for each new behavior and each corrected failure path.

Update the related design document when a contract changes.

Add one `CHANGELOG.md` entry for user-visible behavior.

Run this command before merge:

```powershell
cargo xtask check
```

## Stop conditions

Stop the change when one of these conditions occurs:

- The task requires a new architecture decision.
- A Windows API rule conflicts with an accepted ADR.
- A test needs copyrighted input.
- The public crate contract must break.
- The change expands beyond one task identifier.

Create or update an ADR before work continues.

## Definition of done for the first execution milestone

The milestone completed on August 15, 2026. Every condition passes:

- Windows maps two coherent guest aliases from one physical section.
- The direct backend emits the approved register-only subset.
- The dispatcher enters and leaves generated code under the ADR 0006 ABI.
- The synthetic XBE reaches its entry point.
- Patched kernel thunks call the registered `DbgPrint` and
  `HalReturnToFirmware` exports.
- Execution returns to translated code after each export.
- The runtime exits with `GuestExit { code: 0 }`.
- `cargo xtask check` passes on Windows 11 x86-64.
- No proprietary data exists in the repository.

Run the milestone: `cargo exbawks run .\fixtures\synthetic\minimal-retail.xbe`.

Use [the task board](task-board.md) for issue details and acceptance tests.
