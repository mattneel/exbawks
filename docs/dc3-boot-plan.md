# Dino Crisis 3 boot plan

Status: accepted program plan, 2026-08-15.

## Mission

Boot a locally held retail Dino Crisis 3 image (title ID `0x43430003`, XDK 5558 era,
build 2003-08-04) to its title screen, and add a screenshot command that dumps
deterministic frames for golden testing. The game image never enters the repository;
all automated tests stay synthetic per [testing.md](testing.md) and `AGENTS.md`.

**Execution-engine update ([ADR 0013](adr/0013-whp-execution-tier.md)):** the primary
execution tier is now the Windows Hypervisor Platform (`exbawks-whp`, see
[whp-notes.md](whp-notes.md)), which runs the 32-bit guest natively and deletes this
plan's CPU-emulation and JIT tasks. The interpreter (CPU-001..006) is retained as the
deterministic oracle/golden tier. The kernel HLE (M2–M4), graphics (M5–M6), and
title-screen (M7) milestones below are substrate-independent and unchanged; only the
CPU and JIT tracks are superseded.

## Measured baseline (2026-08-15)

- `inspect`, `thunks`, and `plan` parse the retail XBE cleanly (ADR 0007 works on real data).
- `run` stops at the first entry instruction (`mov ecx, ds:[10118h]` at `0x1A9AD1`) with
  `UnsupportedInstruction`. Memory-operand translation is the first wall.
- The `plan` "Executable" label is a reporting artifact: the emitter translates only the
  register-only prefix of a block, and `BootPlanReport` does not expose coverage.
- The image imports 152 kernel ordinals: 2 real implementations, 10 halting stubs,
  140 unregistered. 19 of the 152 are DATA exports that the patch-every-slot-with-a-gate
  design cannot serve; guest code dereferences those slot values as data pointers.
- The game statically links XDK D3D8, D3DX, XGRPH, DSOUND, DOLBY, and twelve BINK sections.
  Only the kernel is imported, so kernel HLE alone never observes a draw call.
- External evidence: the title is vblank-pacing sensitive; uncapped presents break game
  logic. Present throttling on virtual time is a correctness requirement.

## Strategy decisions

Each decision requires an ADR before dependent code merges. Proposed slugs for
`scripts/new-adr.ps1` are listed with each decision.

1. **Interpreter tier 0, staged JIT.** A complete user-mode x86 interpreter in
   `exbawks-cpu` becomes the run-loop fallback for untranslated instructions and the
   permanent differential oracle for every JIT stage. Boot depth decouples from JIT
   coverage. JIT emission lands per instruction family with differential tests; the
   Windows arena, inline access, and ADR 0004 fault redirection come last as performance
   work. Slugs: `interpreter-tier`, `nonleaf-block-abi` (memory helpers break the
   ADR 0006 leaf-function rule).
2. **Kernel runtime contract.** A kernel-variables guest region at a fixed high VA backs
   the 19 DATA exports (ticking `KeTickCount` cell, `KeTimeIncrement`,
   `LaunchDataPage = NULL`, `XeImageFileName`, zeroed synthetic key blocks, plausible
   version and hardware structs, object-type placeholders). The same ADR fixes KPCR/TIB
   placement, per-thread stacks honoring the XBE stack size, and the `0x8000_0000 | PA`
   contiguous-memory convention. `KernelCallContext` widens to expose address-space
   operations, the object table, the clock, and a pending-guest-call queue for DPC, APC,
   and ISR invocation. The ordinal table is machine-generated from nxdk's CC0-licensed
   `xboxkrnl.exe.def` (371 entries). Decode rule: `Name@N` is stdcall with
   `stack_bytes = N`; `@Name@N` is fastcall with `stack_bytes = max(0, N - 8)` (N counts
   ECX/EDX bytes); undecorated names are cdecl with `stack_bytes = 0`. The image imports
   four fastcall ordinals (87, 160, 161, 250); pin them to `stack_bytes = 0` in tests.
   Slugs: `kernel-guest-map`, `kernel-context`.
3. **Deterministic virtual time.** One nanosecond counter advanced by explicit policy
   (per-block cost, warp-to-deadline when all threads wait). Derives the `KeTickCount`
   cell, `KeQueryInterruptTime`, `KeQuerySystemTime` (epoch pinned to the XBE
   `time_date_stamp`), and RDTSC at 733.33 MHz. No host-time API on any guest path.
   Slug: `virtual-clock`.
4. **Cooperative deterministic scheduler.** Single host thread. Guest threads switch only
   at kernel dispatch points and a fixed block-count quantum. Deterministic FIFO ready
   queues; DPCs drain at switch points; APCs only at alertable waits; timers expire on the
   virtual clock. Slug: `cooperative-scheduler`.
5. **Hybrid graphics interception.** Statically linked D3D runs natively as guest code.
   Trap the NV2A FIFO kick (parse the pushbuffer GET to PUT, lower KELVIN methods to
   `GraphicsCommand` values, report a drained FIFO on readback) and the PCRTC scan-out
   write (present). HLE the zero-signature choke points the image already imports:
   `AvSetDisplayMode` and `KeConnectInterrupt`/`HalGetInterruptVector` for a synthetic
   deterministic vblank. XDK signature scanning and full NV2A LLE are rejected; a fallback
   ladder allows at most single behavior-based choke patches (Swap wait, BinkOpen,
   DirectSoundCreate) if a wait cannot be satisfied hardware-neutrally.
   Slug: `graphics-interception`.
6. **VFS containment and the private-goldens boundary.** Read-only `D:` maps to a
   configured host root; case-insensitive resolution; traversal rejected; deterministic
   metadata. The namespace accepts symbolic links to unbacked devices (XAPI creates `T:`
   and `U:` links at startup) with deterministic Harddisk0 semantics. Golden values for
   the retail title are SHA-256 hashes plus run manifests under git-ignored
   `fixtures/private/goldens/<TitleID>/`, driven by `EXBAWKS_PRIVATE_FIXTURES` and
   `#[ignore]` tests. Public CI receives synthetic equivalents of every subsystem.
   Slugs: `vfs-boundary`, `xe-unload-residency` (keep-resident section policy extending
   ADR 0005 invalidation rules).

## Milestones

Every milestone ends on an observable checkpoint against the retail image (run locally,
never in CI). CPU coverage is front-loaded: XAPI/CRT startup executes `rep` string
operations, `fs:` segment overrides, and RDTSC within its first few thousand
instructions, and CRT floating-point init runs x87 control-word operations before
`main()`.

### M0: truth and triage

- CORE-001: report `translated_instructions` and `static_exit` in `BootPlanReport`.
- KRN-001: verified ordinal table module plus generator from the CC0 nxdk def
  (371 entries; fastcall/stdcall/cdecl decode rule; function-vs-data kind per entry).
- CLI-001: `thunks --check-registry` naming every import as implemented, stub, or
  missing (target: zero missing by end of M3).
- DBG-002: trace filtering, kernel-call name enrichment, end-of-run missing-export
  summary.
- CLI-002: `run --max-blocks` replacing the hardcoded block budget.

Checkpoint: `plan` reports 0 of 6 translated on the retail entry block; `thunks
--check-registry` reports 2 implemented, 10 stubs, 140 missing, all named.

### M1: execute XAPI startup

- ADRs: `interpreter-tier`, `nonleaf-block-abi`, `kernel-guest-map`.
- CPU-001: interpreter memory operands (all addressing modes), 8/16/32-bit ALU, exact
  flags, typed faults.
- CPU-002: interpreter control flow, call/ret/push/pop, stack discipline.
- CPU-003: interpreter `rep` string operations with DF, segment-override effective
  addresses, deterministic CPUID (Pentium III profile), RDTSC from the deterministic
  counter.
- CORE-002: run-loop interpreter fallback (gate probe first, bounded stepping after)
  plus SMC wiring: generation bumps at the shared `GuestMemory` write path so
  interpreter and future JIT stores both invalidate compiled code.
- CORE-003: startup environment: KPCR/TIB page for thread 0 wired to the `fs` base,
  XBE-declared stack size, TLS block reservation.

Checkpoint: the retail image executes its entry stub and XAPI prologue and stops at a
named kernel wall, no longer `UnsupportedInstruction`.

### M2: boot environment

- ADR: `virtual-clock`.
- XBE-001a: certificate parsing (title ID, name, region).
- XBE-001b: section-aware VA-to-file resolver plus TLS directory parsing.
- XBE-001c: library-version table parsing.
- KRN-002: kernel-variables guest region plus data/function-aware thunk patching.
- KRN-003: `VirtualClock` implementation; ticking `KeTickCount` cell; time exports.
- CPU-004: interpreter x87 minimal subset (`fninit`, `fldcw`, `fnstcw`, `fwait`, basic
  load/store/arithmetic).

Checkpoint: data exports read correctly, TLS materialized, clock ticking; the run stops
with a missing export in the heap-init cluster (ordinal 291 or 184).

### M3: kernel core services

- ADRs: `kernel-context`, `vfs-boundary`.
- MEM-006: real physical allocator (free list, top-down constrained contiguous, owner
  tags, statistics).
- MEM-007: VA region map (reserve/commit/decommit/release/unmap, NULL-base search,
  `NtQueryVirtualMemory` answers; frees bump generations of ever-executable pages).
- MEM-008: cache-attribute bits (WC/UC) in free descriptor bits 6-7.
- KRN-004: wire the widened context into `KernelCallContext`.
- HLE-003: Nt virtual memory family.
- HLE-004: Ex pool family.
- HLE-005: object manager: handles, ref counts, namespace, symbolic links including
  links to unbacked devices; object headers in guest memory.
- HLE-006: VFS plus the file I/O export family over `D:`.
- HLE-007: Rtl family (critical sections first; they sit in the heap path).
- HLE-008: `ExQueryNonVolatileSetting` over a synthetic EEPROM blob; Xc SHA-1/HMAC;
  benign Hal/Av/Dbg/Fsc implementations.
- HLE-009: Mm contiguous/GPU family and `XeLoadSection`/`XeUnloadSection`
  keep-resident refcounting (with XBE-001d below; may land in M5 if graphics-blocked).
- HLE-010: registry completeness: every remaining import registered success-returning
  with correct `stack_bytes`; `KeStallExecutionProcessor` advances the clock;
  `RtlUnwind`/`RtlRaiseException` produce typed stops. Exit test: `--check-registry`
  reports zero missing.
- CPU-005: interpreter x87 full fidelity (PC=24 pinned).
- CORE-004: gate dispatch by EIP range (covers `jmp [slot]`, `mov eax,[slot]; call eax`,
  vtable-stored gate pointers).

Checkpoint: XAPI init completes; game `main()` runs; files open from `D:`; the run stops
at thread creation or early device init.

### M4: threads and SIMD

- ADR: `cooperative-scheduler`.
- KRN-005: thread table, scheduler, per-thread stacks/KPCR/TLS, Ps thread exports.
- KRN-006: events and waits (Nt handle-backed and Ke guest-struct dispatcher headers).
- KRN-007: timers plus DPC queue drained at switch points.
- KRN-008: APC delivery at alertable waits plus IoCompletion ports.
- CPU-006: interpreter MMX and SSE1.

Checkpoint: two-thread synthetic tests are byte-identical across runs; the retail image
creates worker threads and stops at a typed unmapped-device access (contiguous alloc,
GPU MMIO, or APU MMIO).

### M5: graphics capture

- ADR: `graphics-interception` (plus `xe-unload-residency` with HLE-009 if not landed).
- MEM-009: MMIO handler registry and dispatch; reserve the NV2A, APU, AC'97, and USB
  OHCI windows so unregistered access is a typed stop.
- XBE-001d: section head/tail shared-page refcount addresses.
- GPU-001: command vocabulary v2 (surface formats, render target/texture/state/viewport,
  indexed and inline draws, frontend guest-state and resource table).
- GPU-002: NV2A pushbuffer parser (pure bytes-to-commands, KELVIN subset,
  `UnknownMethod` census, hostile-input fuzz, portable).
- GPU-003: NV2A MMIO trap wired to the parser, `AvSetDisplayMode` HLE, and
  command-stream observability: `PushbufferKick`/`UnknownMethod` trace events plus
  `run --dump-commands`.
- GPU-004: deterministic vblank via recorded guest ISR fired after flips at virtual
  60 Hz.
- SND-001: APU/AC'97 logging MMIO stub with configurable benign reads; DSound init must
  not hard-stop the run.

Checkpoint: the retail image's first kicked pushbuffer is captured as a serde command
stream. This is the go/no-go signal for the graphics strategy, and the method census
sizes the rasterizer.

### M6: pixels and the screenshot command

- GPU-005: deterministic software rasterizer stage 1 (clear, linear surfaces,
  fixed-point triangle fill, present readback into a host-owned RGBA frame;
  bit-identical on Windows and the Linux portable-logic job).
- GPU-006: present plumbing: `StopReason::FramePresented`, runtime backend selection on
  `Emulator` (flagged public-contract change), a last-presented-frame accessor, and CLI
  `run --until-present N --screenshot out.png` with PNG encoding in the CLI crate.
- QA-002: private compatibility harness (`EXBAWKS_PRIVATE_FIXTURES`, `#[ignore]` tests,
  hash-based goldens, `just dc3-smoke`).
- QA-003: public synthetic goldens for every subsystem; extended fuzz targets. No
  `.bin`-suffixed fixtures outside `fixtures/private`; regenerate the synthetic XBE
  byte-identically with any generator change.
- DBG-003: run manifest as the first trace record (commit, config, input SHA-256,
  clock epoch).

Checkpoint: `run --until-present 1 --screenshot` writes a deterministic PNG.

### M7: title screen

- JIT-004 through JIT-007 and JIT-009 are entry criteria for real-time media (see the
  JIT track); golden capture is wall-clock-insensitive, so correctness runs may stay
  interpreter-only. An FMV decode throughput measurement decides how much JIT must land.
- GPU-007: rasterizer stage 2 scoped by the M5 census (texturing with
  unswizzle-on-sample, blending, whatever the title screen draws).
- SND-002: targeted APU register fakes from SND-001 traces; the budgeted contingency is
  a single behavior-based DirectSoundCreate choke patch.
- INPUT-001: USB OHCI "no devices connected" stub satisfying XInput enumeration.
- FMV ladder, instrumented and in order: play movies for real (decode correctness via
  CPU-006, speed via JIT-009); if blocked, behavior-based BinkOpen fail-clean; last, the
  file-not-found skip experiment.
- Present throttle to virtual 30/60 Hz (pacing is correctness for this title).

Checkpoint: a stable title-screen frame hash recorded under private goldens; the
compatibility entry reaches status `menu` per [compatibility.md](compatibility.md).

## JIT acceleration track

Runs beside M2 through M6 once the `nonleaf-block-abi` ADR lands. Every stage is
differential-tested against the interpreter.

- JIT-004: memory operands via helper calls, with an SMC invalidation differential test.
- JIT-005: full ALU and flags emission.
- JIT-006: control-flow emission plus compile-time `KernelCall` classification (the
  thunk-slot read becomes a recorded page dependency).
- JIT-007: string operations.
- JIT-008: `fs:` base, CPUID, RDTSC emission.
- JIT-009: x87, MMX, SSE1 emission.
- JIT-010: Windows arena activation, inline guest access, ADR 0004 fault redirection,
  view-splitting unmap (with MEM-010 making `Emulator` generic over the address space).
  Post-title-screen; its trigger is the M7 throughput measurement, not the calendar.

Stages 004-007 and 009 sit on the M7 critical path. Only JIT-010 is genuinely deferred.

## Golden testing

Two layers, deterministic by construction (virtual clock, cooperative scheduler,
sorted VFS, fixed-point rasterizer):

1. Command-stream goldens (from M5): serde JSON of the `GraphicsCommand` stream at
   present N; pixel-independent regression net for the CPU, kernel, and pushbuffer
   layers.
2. Frame goldens (from M6): SHA-256 of raw RGBA at present N, plus PNG for humans.

In-repo CI sees synthetic streams and frames only. Retail-title hashes live under
git-ignored `fixtures/private/goldens/<TitleID>/`, keyed by the DBG-003 manifest. A
sanitized subset (input hash, ordinal coverage, status level) graduates to the public
compatibility docs.

## Risks

- Vblank pacing: designed out via virtual time; verified early by the M5 command census.
- FMV wall: intro movies are CPU-decoded Bink (MMX/SSE); the M7 ladder resolves
  skippability empirically; CPU-006 plus JIT-009 is the real fix.
- Audio at the register level is unproven ground for HLE emulators; SND-001's
  trace-first approach scopes it, and the single choke patch is the funded contingency.
- The title screen may be 3D; the M5 census decides rasterizer stage-2 scope before
  pixel work starts.
- Wrong `stack_bytes` values corrupt the guest stack silently; the fastcall decode rule,
  pinned tests, and a debug-mode ESP-balance assertion mitigate.
- SMC staleness: generation bumps must land with the first executed store (CORE-002),
  wired at the shared write path used by both execution tiers.
- Scope creep into LLE: the graphics ADR draws the line; the `UnknownMethod` census is
  the early-warning gauge.
- Handoff-order note: the "no graphics before memory operands translate" rule is relaxed
  for the portable pure-logic subset (GPU-001, GPU-002, GPU-005) once the graphics ADR
  is accepted; the relaxation is recorded in [agent-handoff.md](agent-handoff.md).

## First five pull requests

1. CORE-001: truthful plan report.
2. KRN-001: verified ordinal table plus generator.
3. CLI-001: `thunks --check-registry`.
4. ADR `interpreter-tier`.
5. CPU-001: interpreter memory operands and ALU with the differential harness.
