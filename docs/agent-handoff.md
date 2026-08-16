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
- A JSON Lines trace writer with sequence numbers and opt-in private host
  paths (`DBG-001`).
- Contiguous XBE section mapping with merged shared-page permissions, so
  genuine retail images load (ADR 0007).
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
6. Read `docs/dc3-boot-plan.md`.
7. Select one task from `docs/task-board.md`.

## Immediate objective

Tasks `MEM-001` through `MEM-005`, `JIT-001` through `JIT-003`, `HLE-001`,
and `HLE-002` are complete. The first execution milestone passes on
Windows 11 x86-64.

Active program: boot a retail Dino Crisis 3 image to its title screen with a
deterministic screenshot command. Read [the boot plan](dc3-boot-plan.md).

Milestone M0 is complete (`CORE-001`, `KRN-001`, `CLI-001`, `CLI-002`,
`DBG-002`; ADR 0008 accepted). Milestone M1 is complete except `CORE-003`:
ADRs 0009 and 0010 are accepted, `CPU-001` through `CPU-003` landed the
tier-0 interpreter (memory operands, ALU, control flow, stack, strings,
CPUID, RDTSC), and `CORE-002` wired the run-loop fallback plus shared SMC
generation bumps. The M1 checkpoint is exceeded: the retail image executes
its complete XAPI entry path and stops at
`UnimplementedKernelExport { 255 } (PsCreateSystemThreadEx)`.

Evidence update for planning: XDK 5558's XAPI startup calls
`PsCreateSystemThreadEx` as its FIRST kernel call — before heap init — so
the main guest thread is kernel-created. Thread-creation HLE (M4's `KRN-005`
scope) moves ahead of the M3 heap work in boot order; the minimal viable
form is creating the main thread's context inline and running it, deferring
the full scheduler. `CORE-003` (KPCR/TIB page, XBE-declared stack size)
naturally pairs with it.

`CORE-003` and `KRN-005` are complete (ADRs 0011 and 0012 accepted): the
boot thread has a KPCR/KTHREAD page with an fs base, an XBE-sized stack, and
guard-page-protected child stacks; `PsCreateSystemThreadEx`,
`PsTerminateSystemThread`, and a minimal `NtClose` (first `HLE-005` slice)
are real. The retail image now runs its whole XAPI startup — creating its
worker thread and closing the handle — and stops with
`GuestFault { address: 0 }`: guest code shortly after the `NtClose` return
(last translated block at guest `0x1A6A5A`) transfers control to null, with
no kernel call in between.

Execution-engine pivot: ADR 0013 makes the **Windows Hypervisor Platform the
primary execution tier**, with the interpreter retained as the deterministic
oracle/golden tier and the hand-written JIT track dropped. `exbawks-whp` has
begun (capability doctor; `exbawks doctor` confirms WHP is usable on the
build host). `docs/whp-notes.md` holds the full partition/boot/exit API
reference. Diagnostics: `exbawks coverage` reports the implemented/stub/
missing burndown across CPU/kernel/GPU surfaces (DC3: 24/4/124 of 152 imports).

Two active threads:

1. **WHP-M0 spike** (the make-or-break for the pivot): in `exbawks-whp`, add
   a dynamically-loaded `WhpApi` (all partition/vcpu/memory/register
   functions via `LoadLibrary`+`GetProcAddress` — never raw-dylib, or the CLI
   stops launching on non-WHP hosts), RAII `Partition`/`Vcpu`, `map_gpa`, and
   the 32-bit protected-mode boot register set from `docs/whp-notes.md`
   (CS `0xC09B` / DS `0xC093`, CR0=PE, EFER=0, RFLAGS=2). First test: map a
   page holding `HLT`, boot the vCPU at it, expect `WHvRunVpExitReasonX64Halt`.
   Then the gate-trap: leave `0xFF80_0000+ord*4` unmapped, `call [slot]`,
   expect a `MemoryAccess` exit whose GPA yields the ordinal. Guard tests
   with `cfg(all(windows, target_arch="x86_64"))` and a runtime
   `probe_whp().usable()` skip. Watch the XSAVE-size gotcha and the segment
   attribute bits.

2. **Kernel HLE burndown** (substrate-independent, needed under either
   engine). Done this session, in order the retail image demanded them:
   `CORE-004` (gate dispatch when EIP *enters* the gate region, for the
   `mov reg,[slot]; call reg` and `jmp [slot]` forms, not only a decoded
   `call [slot]`); `ExQueryNonVolatileSetting` (a synthetic NTSC-U EEPROM
   profile); `RtlNtStatusToDosError`; `KeInitializeDpc`/`KeInitializeTimerEx`/
   `KeSetTimer` (dispatcher objects; timers do not fire yet);
   `NtAllocateVirtualMemory` (the user-range reserve/commit allocator — the
   Mm/physical-memory model the earlier wall needed, `HLE-003`); the host file
   device (ADR 0014: `NtOpenFile`/`NtCreateFile`/`NtReadFile`/
   `NtQueryInformationFile` over a sandboxed read-only disc mount, `HLE-004`);
   and the `Mm*` contiguous family (`MmAllocateContiguousMemory`(+`Ex`),
   `MmGetPhysicalAddress`, `MmFreeContiguousMemory`, `MmPersistContiguousMemory`,
   `HLE-009`). A directory/device object now opens as a zero-size marker so the
   disc/HDD presence check passes. The retail image runs its **entire early
   initialization** — EEPROM probe, heap allocation, disc/HDD presence check,
   contiguous GPU-buffer allocation — with no reboot.

   Also landed the disc-metadata exports (`NtDeviceIoControlFile` benign media
   probe; `NtQueryVolumeInformationFile` DVD geometry + nonzero free space +
   CD-ROM characteristics), so the game completes its disc and HDD-partition
   probe.

   **Soft-reboot relaunch implemented (ADR 0015).** After the disc/HDD probe
   the game self-relaunches: a launch helper at guest `0x1A6381` (called via
   `0x1A64F9`) does `MmAllocateContiguousMemory` → `MmPersistContiguousMemory`
   → `HalReturnToFirmware(2)`, setting the `LaunchDataPage` global (ordinal
   164 cell at `0x8010_0080`) to the persisted page at `0x8022_5000`; the page
   holds `dwLaunchDataType=1`, `dwTitleId=0x43430003` (DC3 itself),
   `szLaunchPath=""` — an `XLaunchNewImage(NULL, …)` self-relaunch. `Emulator::
   run` now preserves the persisted regions and the `LaunchDataPage` pointer
   across a reset, reloads the same image, and continues (`relaunch_title`).

   **Reboot loop SOLVED: it was FATX volume geometry.** The decisive evidence
   was the launch data itself, dumped at `HalReturnToFirmware` time: the
   `LAUNCH_DATA_PAGE`'s data area (page + `0x400`) held
   `LD_TO_DASHBOARD { dwReason=1, dwContext=0, dwParameter1=2 }` — reason 1 is
   `XLD_LAUNCH_DASHBOARD_HARDDISK_CLEANUP`, "reboot to the dashboard and free
   up 2 blocks". DC3 was not relaunching to apply settings; it was rebooting
   to a dashboard *cleanup screen* we do not host, because its save-space
   check on `\Device\Harddisk0\partition1\` computed **zero free blocks**.
   Root cause: `NtQueryVolumeInformationFile` (class 3, the exact class DC3
   passes) reported DVD geometry — 2 KiB clusters. Titles convert clusters to
   16 KiB save blocks as `SectorsPerAllocationUnit * BytesPerSector / 16384`,
   which is 0 at 2 KiB clusters, so free = `Avail * 0 = 0`. Reporting real
   FATX geometry (512-byte sectors, 32 sectors/unit = 16 KiB clusters, 1 GiB
   free) clears the check: DC3 now passes its first boot with **no relaunch at
   all** and advances to drive-letter mounting.

   Two earlier misreads, corrected for the record: (1) `[0x2661BC, 0x2661C0)`
   is not a "relaunch gate" — it is `XLaunchNewImage`'s internal pre-launch
   *hook table* (an empty linker-registration section; a non-zero slot is
   **called as a function pointer**, which is why forcing it "past the gate"
   really made XLaunchNewImage call the launch page as code and fault at
   `0x8023_0000`). (2) The launch chain is: game code checks the volume →
   builds a 3072-byte `LAUNCH_DATA` on the stack (`[ebp-0xC00]`) →
   `XLaunchNewImage(NULL, &data)` at `0x1A633F` → worker `0x1AB046` → builder
   `0x1A6381` copies the data to page+`0x400` (`add esi,400h; rep movsd`) →
   reboot at `0x1A651B`. The ADR 0015 relaunch-with-persistence machinery is
   correct and stays (it is how a *real* settings-style relaunch will work);
   it simply no longer triggers on DC3's boot.

   After the geometry fix, landed in wall order: `IoCreateSymbolicLink` /
   `IoDeleteSymbolicLink` (drive-letter mounting through a link table the
   file device consults during resolution); the **writable hard-disk mount**
   (ADR 0016: partition1 → `%LOCALAPPDATA%\exbawks\hdd\<title-id>\`, with
   directory/file creation and `NtWriteFile` — DC3 creates its `TDATA` dir
   instead of reading the disk as full); and `RtlInitAnsiString` /
   `RtlEqualString`. Debug recipe that cracked the reboot wall, for reuse:
   dump the `LAUNCH_DATA_PAGE` **data area** (+`0x400`), not just the header
   — the dashboard reason code names the failing subsystem precisely (it is
   now a permanent `tracing::debug` in `relaunch_title`).

   **Current DC3 wall: the CRT TLS accessor.** At guest `0x1A8F92`:
   `mov ecx, fs:[4]; mov eax, ds:[0x61E828]; mov eax, [ecx+eax*4];
   mov [eax+4], ecx` — the compiler's thread-local-storage pattern. On the
   Xbox the TIB field at `fs:[4]` holds the thread's **TLS-pointer array**
   (desktop MSVC uses `fs:[0x2C]`; the XDK CRT repurposes TIB+4), and
   `ds:[0x61E828]` is `_tls_index`. Our KPCR writes `NtTib.StackBase` there,
   so the game indexes stack garbage and faults at address 4. Fix (CORE-003
   extension, in `threads.rs::build_thread_pages`): allocate a small
   per-thread TLS array page, point `fs:[4]` at it, and make slot
   `[_tls_index]` point to the thread's TLS block (the XBE header names the
   TLS template: raw data start/end, zero-fill size — copy the template per
   thread; the boot thread and created threads both need it). After TLS the
   game should read real assets (`NtReadFile` works), then the long pole is
   graphics HLE (only `NullGraphicsBackend` exists), which the
   screenshot/frame-dump command hangs off.

The retail image stays outside the repository; automated tests remain
synthetic.

Do not start runtime graphics work before memory operands translate. The
portable pure-logic graphics subset (`GPU-001` command vocabulary, `GPU-002`
pushbuffer parser, `GPU-005` software rasterizer) may proceed once the
`graphics-interception` ADR is accepted.

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
