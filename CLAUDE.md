# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Exbawks is a Rust research emulator for the original Xbox, targeting Windows 11 x86-64 only as the
runtime host (Rust 1.97.1, edition 2024, MSVC target). It uses high-level emulation for kernel and
graphics interfaces and `iced-x86` for guest x86 decoding. It is currently a scaffold: block
decoding, translation planning, and cache metadata exist, but **no executable guest code is emitted
yet** — the direct backend returns `CompilationState::Planned` plans and there is no dispatch loop.

`AGENTS.md` is the authoritative instruction file (invariants, unsafe policy, error handling, task
order). Read it, then `docs/agent-handoff.md` (reading order, acceptance criteria, stop conditions)
before implementation work. Pick work from `docs/task-board.md`; each task has explicit acceptance
criteria and a dependency chain (MEM-001..005 → JIT-001..003, XBE-001 → HLE-001..002).

## Commands

```powershell
.\scripts\bootstrap.ps1        # one-time setup: pinned toolchain + cargo fetch (Windows)
python scripts/static-validate.py   # toolchain-free repo validator — run first
cargo xtask check              # required before merge: fmt-check, check, clippy -D warnings, test, doc
cargo exbawks <cmd>            # cargo alias for the CLI (doctor|inspect|decode|plan|thunks|run), --json for machine output
```

- `cargo xtask check` runs, failing fast: `cargo fmt --all -- --check`, `cargo check --workspace
  --all-targets`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace --all-features`, `cargo doc --workspace --all-features --no-deps`.
  Individual steps: `cargo xtask fmt|fmt-check|build|lint|test|doc`.
- Single test: `cargo test -p <crate> <test_name>` (no xtask shortcut exists).
- A `Justfile` mirrors these (`just check`, `just ci`, `just smoke`, `just inspect <xbe>`, …).
- CI smoke test: `cargo exbawks plan .\fixtures\synthetic\minimal-retail.xbe --json`.
- New ADR: `.\scripts\new-adr.ps1 -Slug <kebab-slug> -Title "<title>"`.
- Tracing: `$env:RUST_LOG = "exbawks=debug"` before CLI commands (`exbawks=trace` is very verbose).
- CI also builds and tests the workspace on Linux (`portable-logic` job) — pure logic crates must
  stay portable; only the emulator runtime is Windows-only.

## Static validator trip hazards

`scripts/static-validate.py` enforces rules that are easy to violate accidentally:

- Every text file (`.rs .md .toml .py .ps1 .yml .json .txt`): UTF-8, **final newline, no trailing
  whitespace, no tab characters anywhere**.
- Every `unsafe {` block needs a `SAFETY:` comment within the 3 lines above it.
- `fixtures/synthetic/minimal-retail.xbe` must be byte-identical to the output of
  `scripts/make-synthetic-xbe.py` — regenerate the fixture whenever the generator changes.
- Files ending `.iso`, `.xbe.orig`, `.bin`, `.rom` are forbidden repo-wide; local test inputs go in
  git-ignored `fixtures/private/`. Never commit proprietary Xbox data (games, BIOS, keys), and no
  automated test may require it.
- Local Markdown links must resolve; `mod foo;` declarations must have matching files.

## Architecture

Crate dependency direction (lower crates must never depend on `exbawks-core`):

- `exbawks-types` — root; shared newtypes only. `GuestVa` / `GuestPa` / `GuestPage` / `GuestRange`
  are distinct types so virtual-vs-physical confusion is a compile error; host pointers never enter
  guest state. Also `MemoryPermissions`, `AccessKind`, `BackendKind`, `BuildFlavor`.
- `exbawks-platform` — owns all host OS calls and nearly all unsafe code
  (`virtual_memory/imp/windows.rs`: `VirtualAlloc2` placeholders, pagefile sections,
  `MapViewOfFile3` replace-placeholder views; `code_memory.rs`: W^X code buffers — all
  RAII-wrapped). The only other unsafe module is `exbawks-jit/src/dispatch.rs`, which
  enters sealed code buffers under the ADR 0006 block ABI.
- `exbawks-xbe` — checked parsing of untrusted XBE files; every read is bounds-checked, entry point
  and kernel-thunk address are XOR-obfuscated per build flavor (retail/debug) and must decode inside
  the image.
- `exbawks-memory` — guest mappings and page metadata. `PageTable` (page_table.rs) is a sidecar of
  2^20 atomic u64 descriptors (kind, permissions, physical page, MMIO id, generation, watch flags)
  plus per-**physical**-page u16 generation counters; mutations validate the whole range before
  writing anything (failed ops leave the map intact). `SoftwareAddressSpace` (software.rs) is the
  deterministic portable MMU used by tests and the current runtime; `WindowsArenaPlan`
  (windows_plan.rs) plans the future 4 GiB arena where host address = arena base + guest VA.
- `exbawks-cpu` — `CpuState` (pure data, `#[repr(C, align(64))]`) and `BasicBlockDecoder` over
  iced-x86; blocks stop at control flow or configured limits.
- `exbawks-jit` — `CodegenBackend` trait (backend.rs): `DirectRewriteBackend` (plan-only today) and
  a `CraneliftBackend` placeholder. `CodeCache` keys blocks by (guest start, address-space epoch,
  backend) and revalidates against physical-page generations — code invalidation is by physical
  page (ADR 0005) because guest code can execute via one virtual alias and be written via another.
- `exbawks-kernel` — kernel HLE: `KernelExport` trait dispatched by ordinal through
  `KernelRegistry`; exports get `&mut CpuState` plus a checked `GuestMemory` handle and return
  NT-style `KernelStatus` codes. Thunk *patching* does not exist yet — `exbawks-core/src/thunk.rs`
  only reads the XBE kernel import table.
- `exbawks-gpu` — graphics HLE: `GraphicsFrontend` validates command ordering and feeds
  host-neutral `GraphicsCommand`s to a `GraphicsBackend` (only `NullGraphicsBackend` exists).
- `exbawks-debug` — `TraceSink` trait + `TraceEvent`s (only `BlockEnter` is emitted today).
- `exbawks-core` — composition root only; must not absorb subsystem logic. `Emulator::load_xbe`
  maps headers/sections into a fresh `SoftwareAddressSpace` and bumps the address-space epoch
  (invalidating all cached blocks by key); `plan_entry_block` is the full implemented pipeline:
  fetch → decode → cache lookup → capture physical-page dependencies → compile plan → report.
- `apps/exbawks-cli` — clap frontend over core; `BootPlanReport` is the shared text/JSON output.

Key design documents: `docs/architecture.md`, `docs/memory.md` (arena + page-table design),
`docs/jit.md`, `docs/codegen-contract.md` (draft ABI for generated code — material changes need a
new ADR), and `docs/adr/` for accepted decisions.

## Invariants that shape changes

From `AGENTS.md` — the full list lives there:

- Track translated-code dependencies by guest **physical** page; keep normal RAM access off the
  slow path; MMIO/invalid pages stay inaccessible in the arena.
- HLE code must never depend on low identity mappings; restore host thread state before calling
  into Rust or Windows helpers; never execute writable code-cache pages (W^X).
- Malformed guest input must never panic — return typed errors (`thiserror`) from library crates,
  add context (`anyhow`) only at application boundaries; no `unwrap` in production code.
- Unsafe code goes in the smallest possible module with a `SAFETY:` comment stating pointer
  lifetime, alignment, mapped range, and thread requirement.

## Process

- One task ID per PR; add tests with each behavior change (synthetic data only — unit tests build
  XBE images in memory); update the related design doc when a contract changes; add a CHANGELOG.md
  entry for user-visible changes; update `docs/agent-handoff.md` after substantial tasks.
- Stop and write an ADR first when: a change needs a new architecture decision, conflicts with an
  accepted ADR, would require copyrighted test input, breaks a public crate contract, or grows
  beyond one task ID.
- Commit style: imperative summary under 72 chars with a subsystem prefix, e.g.
  `memory: add physical alias invalidation`.
