# Changelog

All notable changes will appear in this file.

The project follows Keep a Changelog structure before its first release.

## Unreleased

### Added

- Host memory geometry queries; `exbawks doctor` reports the host page size
  and allocation granularity (`MEM-001`).
- Single-owner placeholder split and coalesce operations for the sparse
  Windows arena (`MEM-002`).
- A 4 GiB-aligned high guest arena reservation with traced base selection
  (`MEM-003`).
- Coherent Windows RAM aliases through pagefile section views with sidecar
  page-table records and placeholder-restoring unmaps (`MEM-004`).
- A Windows `GuestMemory` backend with checked reads, writes, and fetches,
  plus generated equivalence tests against the software backend (`MEM-005`).
- Single-owner executable code buffers with separate write and execute
  phases, sealing, and instruction-cache flushes (`JIT-001`).
- Register-only direct emission and a dispatcher under the ADR 0006 block
  ABI, verified against an interpreter oracle; `exbawks plan` now reports
  the `Executable` state on Windows (`JIT-002`).
- Immutable sorted source ranges and fault-site records for every emitted
  block, with binary-search host-to-guest lookups (`JIT-003`).
- Kernel thunk gate patching at load time and runtime dispatch of gate
  calls to registered HLE exports; `exbawks thunks` reports the table as
  parsed before patching (`HLE-001`).
- The startup kernel export set with real `DbgPrint` and
  `HalReturnToFirmware` implementations, a guest stack for the first
  synthetic thread, and an execution loop; `exbawks run` now executes the
  boot flow and reports the stop reason (`HLE-002`).
- The synthetic fixture now contains the complete first-milestone boot
  title, so `exbawks run` on the fixture exits with `GuestExit`.
- A JSON Lines trace writer with sequence numbers and opt-in private host
  paths; `exbawks run --trace <file>` records block entries, kernel calls,
  and the stop reason (`DBG-001`).
- `exbawks plan` now reports `translated_instructions` and `static_exit`, so
  an `Executable` artifact that translates only a prefix of the block is
  visible instead of reading as full coverage (`CORE-001`).
- A verified kernel export ordinal table (371 entries: name, function or data
  kind, calling convention, gate stack bytes) generated from the vendored
  CC0-licensed nxdk def file, with pinned decode-rule tests (`KRN-001`).
- `exbawks thunks --check-registry` names every imported ordinal and reports
  it as implemented, stub, or missing with summary counts — the standing
  triage tool for kernel HLE burn-down (`CLI-001`).
- `exbawks run --max-blocks <count>` replaces the fixed one-million block
  budget (`CLI-002`).
- Trace filtering and kernel-call name enrichment: `exbawks run
  --trace-filter kernel,stop` restricts JSON Lines records to selected event
  kinds, kernel-call records carry verified export names, and kernel-related
  stop reasons print their export name (`DBG-002`).
- The tier-0 interpreter's first stage (ADR 0008): `exbawks_cpu::step`
  executes memory-operand and integer-ALU instructions at 8, 16, and 32-bit
  widths over checked guest memory with architectural flag semantics and
  typed faults (`CPU-001`).

### Fixed

- The loader validates every header and section range against the declared
  image window in 64-bit arithmetic, so a malformed XBE with a high base
  address or an oversized image returns a typed error instead of panicking
  on overflow, and a section outside the image window is rejected (ADR 0007
  point 5).
- The loader accepts real retail XBEs: sections are byte-contiguous rather
  than page-aligned, so the header/section union maps as guest RAM and each
  page takes the merged permission of the sections touching it, honoring the
  head and tail read-only flags on shared pages (ADR 0007). Genuine retail
  images now load, decode, and execute up to the first unsupported
  instruction.
- Kernel export status now reaches guest EAX; the runtime places the
  returned `KernelStatus` in EAX after each gate call.
- Registered but unimplemented kernel stubs halt the run with a controlled
  `UnimplementedKernelExport` stop instead of continuing past an unbalanced
  guest stack.
- `indirect_call_slot` no longer misclassifies the far `call m16:32` form as
  a near kernel gate call.
- Block fetch spans contiguous mapped pages, so an instruction straddling a
  page boundary decodes instead of aborting the run.
- Initial Rust workspace scaffold.
- XBE parser and synthetic tests.
- Software guest address space.
- CPU block decoder.
- Translation planner and code-cache model.
- Kernel and graphics HLE interfaces.
- Windows virtual-memory wrappers.
- CLI, CI, documentation, and agent handoff.
- Coding-agent task board with dependency and acceptance gates.
- Static repository validator and reproducible public XBE fixture.
- Security, compatibility, configuration, codegen, and release policies.
