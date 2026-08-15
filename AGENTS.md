# Agent Instructions

## Mission

Build Exbawks as a Windows-first original Xbox emulator in Rust.

Keep each change small, testable, and reviewable. Preserve the architecture boundaries in this repository.

## Baseline

Use Windows 11 on x86-64 as the runtime baseline.

Use Rust 1.97.1 and the Rust 2024 edition.

Use `iced-x86` for guest x86 decoding and instruction analysis.

Use direct instruction rewriting as the first codegen backend. Keep the backend trait ready for Cranelift.

## Required invariants

1. Treat guest virtual addresses, guest physical addresses, and host pointers as different types.
2. Track translated code dependencies by guest physical page.
3. Keep normal RAM access outside the software page-table slow path.
4. Keep MMIO and invalid pages inaccessible in the mapped Windows arena.
5. Keep HLE code independent from low identity mappings.
6. Restore host thread state before any Rust or Windows helper call.
7. Do not execute writable code-cache pages.
8. Do not accept unchecked ranges from an XBE file.

## Crate boundaries

`exbawks-types` contains shared value types only.

`exbawks-platform` owns host operating-system calls.

`exbawks-memory` owns guest mappings and page metadata.

`exbawks-cpu` owns guest architectural state and decoding.

`exbawks-jit` owns translation plans, emitted code, and cache state.

`exbawks-kernel` owns kernel HLE dispatch and kernel objects.

`exbawks-gpu` owns graphics HLE commands and host backends.

`exbawks-core` composes the subsystems. It must not absorb subsystem logic.

## Unsafe code

Put unsafe code in the smallest possible module.

Add a `SAFETY:` comment before each unsafe block.

State the pointer lifetime, alignment, mapped range, and thread requirement.

Do not expose a raw host pointer when an address or slice type works.

Do not use `transmute` for file parsing.

## Error handling

Return typed errors from library crates.

Add context at application boundaries.

Do not use `unwrap` in production code.

Use `expect` only for an invariant that a local constructor proves.

## Tests

Add a unit test for each parser boundary and bitfield operation.

Add a synthetic integration test for each boot-flow milestone.

Do not require commercial XBE files in automated tests.

Add a regression test before each bug fix when practical.

## Documentation

Update an ADR when a change modifies an accepted architecture decision.

Update the roadmap when a milestone changes state.

Update `docs/agent-handoff.md` after each substantial task.

Keep technical sentences direct and short.

## Required checks

Run these commands:

```powershell
python scripts/static-validate.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc --workspace --no-deps
```

The `cargo xtask check` command runs the same sequence.

## First task sequence

Use this order unless a blocking defect requires another order:

1. Finish the Windows sparse arena mapper.
2. Add host page protection changes and alias bookkeeping.
3. Add physical-page code invalidation.
4. Emit and execute register-only translated blocks.
5. Add memory operand rewriting through the selected address mode.
6. Add fault-site metadata and MMIO redirection.
7. Patch XBE kernel thunks into HLE gates.
8. Implement the minimum kernel exports for a synthetic title.

Read [the agent handoff](docs/agent-handoff.md) for exact acceptance criteria.
