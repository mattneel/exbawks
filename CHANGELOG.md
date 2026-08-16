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
- Interpreter control flow and stack traffic: near jumps, conditional
  branches, call/ret with argument cleanup, push/pop in both widths,
  PUSHAD/POPAD as single accesses, PUSHFD/POPFD with a writable ID flag for
  CPUID probes, LEAVE, and the LOOP/JECXZ family (`CPU-002`).
- Interpreter string operations with repeat prefixes and DF (partial
  progress commits across faults, as hardware restarts), a deterministic
  Pentium III CPUID profile (SSE yes, SSE2 no), and RDTSC backed by a
  virtualized per-instruction counter in `CpuState` (`CPU-003`).
- The run loop falls back to the tier-0 interpreter for untranslatable
  instructions (gate probe first), guest writes bump physical-page
  generations in both memory backends so self-modifying code invalidates
  cached blocks, and invalid guest accesses stop with the new
  `StopReason::GuestFault`. The retail Dino Crisis 3 image now executes its
  XAPI entry path and stops at its first real kernel wall,
  `PsCreateSystemThreadEx` (`CORE-002`).
- The boot thread receives a KPCR/TIB and KTHREAD page (fs base wired for
  XAPI startup) and a stack sized from the XBE header (`CORE-003`); a
  `KernelServices` context (ADR 0012), a cooperative thread table (ADR 0011),
  and real `PsCreateSystemThreadEx`/`PsTerminateSystemThread` exports let the
  retail image create its worker thread and advance to `NtClose` (`KRN-005`).
- A minimal object handle table with a real `NtClose` export closes the
  thread handle instead of halting; the retail image advances past handle
  cleanup (first slice of `HLE-005`; the namespace, symbolic links, and Ob*
  exports follow).
- The Rtl critical-section family (`RtlInitializeCriticalSection`,
  `RtlEnterCriticalSection`, `RtlLeaveCriticalSection`) maintains the guest
  `RTL_CRITICAL_SECTION` lock and recursion counts without blocking, which is
  correct under the cooperative single-thread scheduler; the retail image
  advances past its heap-init critical section (first slice of `HLE-007`).
- The boot KPCR now populates `NtTib.Self` and the `KPCR.Prcb` pointer
  (`fs:[0x20]`) at the embedded KPRCB, whose `CurrentThread` field XAPI reads;
  the retail image advances past its `KeGetCurrentThread`-style Prcb walk
  (`CORE-003`).
- A minimal synthetic kernel PE image maps at the kernel's fixed base
  `0x8001_0000` so titles that read the kernel image directly (parsing its
  PE section table) find a valid DOS/PE header; the kernel object region
  moved above it to avoid colliding with the kernel's address. The retail
  image advances past its kernel-image section parse (`CORE-003`).
- A reusable `SuccessExport` backs benign kernel exports that are safe no-ops
  on the boot path; `HalRegisterShutdownNotification` uses it, so the retail
  image advances past its shutdown-notification registration (`HLE-010`).
- `KeInitializeDpc` and `KeInitializeTimerEx` initialize the guest DPC and
  timer dispatcher objects; the objects do not fire yet (the DPC/timer
  machinery is later work) but their structs are consistent so boot proceeds
  (`HLE-007`).
- The run loop dispatches a kernel export when the guest's EIP enters the
  gate region, not only when it decodes `call [slot]` at the caller. Titles
  that call through a register (`mov reg,[slot]; call reg`) or tail-jump
  (`jmp [slot]`) now resolve to the right ordinal instead of faulting inside
  the gate region; a gate address with no registered export reports a named
  missing export. The retail image advances to `ExQueryNonVolatileSetting`
  (`CORE-004`).
- `ExQueryNonVolatileSetting` returns a synthetic NTSC-U console profile for
  the EEPROM settings a title reads at startup (language, video/audio flags,
  region), writing the value type and length and honoring the caller's
  buffer size; there is no real EEPROM, so guest-visible identifiers stay
  zeroed (ADR 0010). The retail image advances past its EEPROM probe to
  `RtlNtStatusToDosError` (`HLE-011`).
- `RtlNtStatusToDosError` translates the NTSTATUS codes titles hit on their
  error paths to Win32/DOS error codes, returning the result in EAX, with a
  generic fall-through matching the kernel (`HLE-007`).
- `KeSetTimer` records a timer's due time and DPC and reports the timer was
  not already queued (`FALSE`). Under the cooperative scheduler (ADR 0011)
  the timer does not fire yet — the firing machinery is later work — so a
  title that arms fire-and-forget timers proceeds. The retail image advances
  to `NtAllocateVirtualMemory` (`HLE-007`).
- `NtAllocateVirtualMemory` reserves and commits guest virtual memory through
  a new `KernelServices::allocate_virtual_memory` seam backed by a user-range
  allocator: commit maps physical pages with the requested `PAGE_*`
  protection, a reserve-only request records the range without backing it,
  and kernel-chosen (`BaseAddress == 0`) placements bump a user-space cursor
  in the `0x1000_0000`–`0x7F00_0000` window (ADR 0010). The real reserve/
  commit region map and page reclamation remain later work (MEM-006/007). The
  retail image allocates its heap and advances to `NtOpenFile` (`HLE-003`).
- A host-backed file device (ADR 0014): `NtOpenFile`, `NtCreateFile`,
  `NtReadFile`, and `NtQueryInformationFile` read the game's own files through
  a new `KernelServices` file surface. The device mounts the image's directory
  as the read-only game disc (`\Device\CdRom0\`, `\Device\Harddisk0\Partition1\`,
  `D:`); guest paths resolve inside the mount only — a component-depth sandbox
  rejects `..` escapes, absolute and UNC forms, and embedded NULs, with a
  canonicalized containment re-check. No proprietary data enters the
  repository (the mount is a runtime path; tests use synthetic files). The
  retail image opens the disc device and advances to
  `MmAllocateContiguousMemory` (`HLE-004`).
- The `Mm*` contiguous-memory family (`HLE-009`): `MmAllocateContiguousMemory`
  and `MmAllocateContiguousMemoryEx` serve physically contiguous GPU/DMA
  buffers from the kernel window through a new
  `KernelServices::allocate_contiguous` seam, `MmGetPhysicalAddress` masks a
  window address to its physical address (ADR 0010), `MmFreeContiguousMemory`
  is a no-op until page reclamation lands (MEM-006), and
  `MmPersistContiguousMemory` is a benign no-op. The retail image allocates
  and persists its launch-data page and reaches its startup relaunch
  (`HalReturnToFirmware` quick-reboot), so `exbawks run` now drives the retail
  image through its entire early initialization to a controlled exit.
- Implementation-burndown diagnostics: `exbawks coverage` reports
  implemented/stub/missing counts across the CPU, kernel, and GPU surfaces
  (with `--xbe` to scope the kernel surface to one image's imports), and a
  run that stops at a coverage gap renders an ariadne-annotated call site
  with miette-style diagnostics.
- The `exbawks-whp` crate begins the WHP execution tier (ADR 0013): a
  capability doctor that dynamically loads `WinHvPlatform.dll` and reports
  library/hypervisor/tier availability without breaking the CLI on hosts
  without WHP. `exbawks doctor` now reports WHP status.
- A kernel-variables region backs the DATA exports (ADR 0010): the loader
  patches each imported data-ordinal thunk slot with a pointer to a live
  variable (`KeTickCount` cell (static until the virtual clock lands),
  `KeTimeIncrement`, `XeImageFileName` ANSI string, `XboxKrnlVersion`, zeroed
  keys and object types, and `LaunchDataPage` NULL) while function slots keep
  gates (`KRN-002`). The
  boot thread now returns off its stack to a thread exit and the scheduler
  switches to the game's created thread, so the retail image advances past
  its bootstrap to `RtlEnterCriticalSection`.

### Added

- Soft-reboot title relaunch (ADR 0015): a guest that self-relaunches through
  `HalReturnToFirmware(quick reboot)` carrying a persisted `LAUNCH_DATA_PAGE`
  no longer ends the run. `HalReturnToFirmware` reboot routines raise the new
  `StopReason::Reboot`; `MmPersistContiguousMemory` records the persisted
  region through a new `KernelServices::persist_memory`; and `Emulator::run`
  preserves the persisted regions and the `LaunchDataPage` pointer across a
  machine reset, reloads the same image, and continues — so the relaunched
  title reads its launch data. A relaunch that persists byte-identical launch
  data is detected as a reboot loop and stops with `Reboot` rather than
  spinning. The retail image now relaunches itself and the second boot reuses
  the preserved launch data (skipping its `MmAllocateContiguousMemory`); it
  still reboots for an upstream reason the emulator does not yet model, which
  the loop detector reports.
- Disc device metadata exports (`HLE-004`): `NtDeviceIoControlFile` answers a
  title's disc IOCTLs (media detection) with a benign success, and
  `NtQueryVolumeInformationFile` reports a read-only 2 KiB-sector DVD's
  geometry, nonzero free space, and CD-ROM device characteristics. The retail
  image completes its disc and HDD-partition probe and reaches its startup
  self-relaunch (a `HalReturnToFirmware` quick-reboot through a launch-data
  helper, the next milestone).

### Added

- Object-namespace symbolic links (`HLE-005` slice): `IoCreateSymbolicLink`
  and `IoDeleteSymbolicLink` record guest drive-letter mounts (`\??\D:` →
  `\Device\CdRom0`, …) in the host file device, which rewrites a matching
  link prefix during path resolution, so opens through a drive letter reach
  the right mount.
- A writable hard-disk mount (ADR 0016): `\Device\Harddisk0\Partition1`
  resolves to a per-title host directory
  (`%LOCALAPPDATA%\exbawks\hdd\<title-id>\`) where the guest can create
  directories (`FILE_DIRECTORY_FILE`), create files, and write
  (`NtWriteFile` over a new `KernelServices::write_file`). The disc mount
  stays read-only, and the ADR 0014 sandbox applies to creation paths. The
  retail image creates its `TDATA` save directory instead of reading the
  disk as full.
- `RtlInitAnsiString` and `RtlEqualString` (`HLE-007`). The retail image
  advances through its title-data bootstrap to its CRT thread-local-storage
  accessor, the next wall (a per-thread TLS array behind `fs:[4]`).

### Fixed

- `NtQueryVolumeInformationFile` size queries now report FATX hard-disk
  geometry (512-byte sectors, 32 sectors per unit — 16 KiB clusters) instead
  of DVD geometry. Titles convert clusters to 16 KiB save blocks as
  `SectorsPerAllocationUnit * BytesPerSector / 16384`, which truncated to
  zero at 2 KiB clusters, so every free-space check saw an empty disk. This
  was the retail image's reboot loop: its launch data was
  `LDT_TO_DASHBOARD` reason 1 (hard-disk cleanup, 2 blocks needed) — it was
  rebooting to a dashboard cleanup screen we do not host. With real
  geometry the retail image passes its save-space check on the first boot,
  never relaunches, and advances to drive-letter mounting
  (`IoCreateSymbolicLink`).
- The host file device opens a directory or device object (the disc root, an
  HDD partition) as a zero-size marker instead of reporting "not found", so a
  title's disc/HDD presence check passes. The retail image now clears its
  presence check — no longer taking the `HalReturnToFirmware` reboot — and
  advances to a disc device IOCTL (`NtDeviceIoControlFile`).
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
