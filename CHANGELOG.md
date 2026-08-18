# Changelog

All notable changes will appear in this file.

The project follows Keep a Changelog structure before its first release.

## Unreleased

### Added

- ADR 0020 proposes a hardware graphics backend. Neither xemu nor
  Cxbx-Reloaded rasterizes in software: xemu registers a renderer over
  OpenGL or Vulkan and generates shaders from the console's vertex programs
  and register combiners, and the graphics processor draws the pixels. That
  is the difference in kind no arrangement of a scalar loop reaches, and
  the pushbuffer decode this project already has is the input such a
  backend needs. The software rasterizer stays as the reference the goldens
  are recorded from.
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
  accessor.
- Thread-local storage at the stack top (`CORE-003`): the loader parses the
  XBE's `IMAGE_TLS_DIRECTORY` and every thread reserves and initializes its
  TLS area just below `NtTib.StackBase`, matching the XDK CRT contract
  (`_tls_index = -size/4`; the block pointer lives at `[StackBase - size]`),
  with the initial stack pointer kept below the reserve. Decoded from the
  image's own CRT: the earlier "TLS array at `fs:[4]`" model was wrong.
- `NtQueryInformationFile` answers `FileNetworkOpenInformation` (times,
  sizes, attributes), which XAPI's save bootstrap reads after creating
  `TitleMeta.xbx`; a failure there read as a disk problem.
- `XeLoadSection` and `XeUnloadSection` maintain a demand-loaded XBE
  section's reference counts; the bytes are already resident because the
  loader maps the whole section union (ADR 0007).
- `NtOpenSymbolicLinkObject` and `NtQuerySymbolicLinkObject` read back the
  drive-letter links a title created, through link-object handles on the
  file device.
- The interpreter executes the x87 control-plumbing subset (`fninit`,
  `fnstsw`, `fnstcw`, `fldcw`, `fnclex`, `wait`), enough for CRT
  floating-point presence probes; arithmetic x87 remains for the WHP tier
  (ADR 0013).
- `KeQuerySystemTime` reports a deterministic virtual clock derived from the
  virtualized time-stamp counter (first slice of `KRN-003`), and
  `MmQueryStatistics` reports a synthetic retail memory profile.
- Real `NtCreateEvent` and `NtSetEvent` maintain guest event objects under
  the cooperative scheduler (events never block; ADR 0011).
- The run loop advances the `KeTickCount` cell deterministically (one
  millisecond per 4096 executed blocks, `KRN-003`), so titles that pace
  themselves by polling the tick counter make progress instead of spinning
  forever.
- A repeated string instruction yields back to the run loop every 64 Ki
  iterations with its committed partial progress (hardware REP is
  interruptible; EIP stays on the instruction), so one giant guest `memset`
  can no longer freeze the run loop, the virtual clock, or the block budget.
- The run loop emits an info-level heartbeat (executed blocks, EIP, TSC)
  every 16 Mi blocks, so a long or looping run stays observable.

- The WHP-M0 spike (ADR 0013), verified on hardware: `exbawks-whp` gains a
  dynamically loaded platform API (`LoadLibraryW`-resolved, so hosts without
  the optional feature still run the CLI), an RAII `Machine` (partition +
  one vCPU, strict bring-up order), page-aligned `HostRegion` guest RAM with
  `map_gpa`, the flat 32-bit protected-mode boot state, and a decoded exit
  surface. Hardware tests prove the tier's two load-bearing properties: a
  guest `HLT` produces the `X64Halt` exit, and a read of the unmapped kernel
  gate region produces a `MemoryAccess` exit carrying the exact gate GPA —
  the mechanism the HLE dispatch rides on. An adversarial review against the
  installed SDK headers then caught mistranscribed register names (`Cr0`
  actually addressed the TPR; `Efer` is `0x2001`, not `0x501`), a `map_gpa`
  that let safe code free a region the hypervisor still mapped (the machine
  now takes ownership of mapped regions), and non-page-granular region sizes
  the platform rejects (now rounded); all fixed and re-proven on hardware.

- The NV2A register combiners run for every shaded pixel, replacing the
  fixed texture-times-diffuse the rasterizer assumed. A title programs up to
  eight general stages and a final one; each reads four variables from a
  small register file, maps them through signed or unsigned transforms, and
  writes back products, sums, muxes, or dot products with a scale and bias.
  Dino Crisis 3's title screen was rendering nearly black, because the fixed
  modulate multiplied by a diffuse color its own program never applies. Its
  frame now comes out lit, and the recorded golden digest moves with it.

### Changed

- Drawing spreads a surface's rows across the host's processors, which took
  a run of the retail title from 79 seconds to 26 and its unfiltered form
  from 57 to 22. The frame is byte-for-byte the one a single thread drew —
  the digest is unchanged — because a row's pixels depend only on the
  triangles covering them and on what is already beneath them, so every
  pair of triangles that meet stay on one thread and in submission order.

  Two things had to be true first. Guest RAM is now held as dwords the
  threads share rather than as a slice one of them owns, which is what says
  the rows are disjoint; on x86-64 a relaxed load or store of an aligned
  dword is an ordinary move, so nothing pays for the possibility. And the
  bands run on a pool that outlives the draw: a title submits a draw every
  few hundred microseconds, and creating threads for each of the 111,000 of
  them cost more than the drawing did — the first attempt this way was
  barely faster than one thread.

  How many bands a draw is divided into comes from its triangles' own areas
  rather than the box around them, because a draw of narrow slivers spans
  as many rows as one of full-width bars and is a fraction of the work.

  Pixels now cost 16 seconds of a 26-second run, down from about 71. What
  is left is 6 seconds of guest and geometry and 5 of per-triangle setup,
  and the drawing that remains scales poorly with more threads — random
  texture reads make it memory-bound, which is the wall a software
  rasterizer reaches and the reason ADR 0020 exists.

- `exbawks run --unfiltered` takes the nearest texel instead of blending
  four, which draws about a third faster and looks coarser. Blending is
  what this title asks for and costs 24 of the 79 seconds a run spends;
  trading it is a choice for someone who wants to play rather than
  something decided for them. It changes the frame, so a run using it may
  not record a golden.

  Where a run's time goes, measured by removing one stage at a time:
  8 seconds of guest and geometry, 24 in the combiner, 24 in filtering,
  about 13 in the rest of sampling, and about 9 in the remaining
  per-pixel work. Ninety percent of a run is pixels.

- Drawing is about twice as fast: a full run of this title went from 186
  seconds to 85. The graphics engine now takes guest RAM once for a whole
  batch of submissions and addresses it physically, rather than going
  through the address space for every four bytes. Each of those accesses
  validated a range, took a lock, and walked the page table twice — the
  right cost for a guest instruction touching a word and the wrong one for
  a rasterizer reading eight texels and writing a pixel, 388 million times
  a run. The frame is identical, digest and all.

- A mip level's address is worked out when a texture is bound rather than
  on every texel fetch, where it had been adding up every earlier level's
  size each time.

### Added

- The pseudo-handle a thread passes to mean itself resolves to whichever
  thread is running, rather than being looked up in the handle table and
  refused. Cxbx-Reloaded special-cases the same constant for the same
  reason: it is never in the table, because it is what a caller passes
  instead of its own handle.

- `ObReferenceObjectByHandle` hands back the object a handle names rather
  than the handle itself. A caller reads fields off what it is given, and
  a thread handle is a small number, so dereferencing one faulted on a low
  address — which is exactly what the title did after a press moved it
  through its menu. A thread handle now resolves to that thread's own
  control block, and a handle this runtime keeps no body for is refused
  rather than answered with something that cannot be dereferenced.

- `NtYieldExecution`, `ObReferenceObjectByHandle`, and
  `ObfDereferenceObject`. These are the exports the title asks for once a
  button is pressed, and each was a wall in turn: pressing start took it
  down a code path it had never reached, where it yields to another thread
  and resolves a handle to an object. The object manager keeps no object
  bodies, so a handle stands for itself and is what a reference hands
  back — the only token this runtime has, and the only one it is handed
  back later.

  **With these the title reaches its difficulty menu.** A press on a real
  controller now takes it past the title screen it had been sitting on for
  the whole project.

- `exbawks run --display` shows what the title is drawing, in a window,
  while it runs. The window has its own thread and message loop, because a
  window that is not pumped stops repainting and the system declares it
  hung; what crosses between it and the run is a buffer of pixels and two
  flags, so the run never touches a window handle. Closing the window
  stops the run, which is how a person ends one they are watching. It
  costs about three percent of a run's time.

- `exbawks run --gamepad-live` reads a real controller from the host and
  feeds it to the emulated one. A DualSense, DualSense Edge, or either
  DualShock 4 is opened over raw HID, read on its own thread because a
  report read blocks until the device sends one, and translated into the
  console's twenty-byte report — a translation rather than a
  pass-through, since none of those controllers presents the descriptors
  or report layout the title's driver is written against. The run reports
  which buttons it saw pressed, so a person can tell whether their
  controller reached the guest.

  A run reading a live controller is not reproducible by construction and
  may not record a golden (ADR 0019). The default remains no controller at
  all, and `--gamepad` without `--gamepad-live` attaches one that presses
  nothing, both of which keep a run a pure function of the image.

- **The title enumerates the gamepad.** Its own USB driver now walks the
  whole sequence and is answered at every step: the device descriptor,
  `SET_ADDRESS`, the configuration descriptor, `SET_CONFIGURATION`, a
  report read and a capability read — each asked exactly once, with no
  retries and no port cycling — and it then polls the interrupt endpoint
  for input about sixteen hundred times a run.

  Three defects in this engine were in the way, and all three were about
  telling the driver how much data a transfer moved. A descriptor that
  filled its buffer reports an empty buffer pointer and one that did not
  leaves it at the next byte it would have written; this engine reported
  empty for both, so a thirty-two byte answer to an eighty-byte request
  claimed eighty bytes had arrived. The setup stage reported a stale
  pointer, from which the driver computed a negative length and failed its
  own byte-count check. And a request carrying no data still ends with a
  zero-length stage, which this engine declined to complete at all — after
  `SET_ADDRESS` the driver polled for that completion five thousand times
  and never got it.

  The controller also no longer overwrites a completion list the driver
  has not taken yet, which the hardware may not do either: doing so loses
  every completion on it and asks for an interrupt per frame that the
  driver can never catch up with.

- A retired transfer advances the data toggle and carries it back to its
  endpoint, and the done queue is handed over only once the shortest delay
  a retired descriptor asked for has run out. Both are how the hardware
  behaves, checked against xemu's controller: it marks the descriptor's
  toggle explicit, flips it, and copies it into the endpoint's carry bit,
  and it defers the completion interrupt by the descriptor's own delay
  field rather than raising it on every transfer.

- Completed transfers are handed to the driver through the communication
  area, as the hardware does, rather than left in the done-head register:
  a driver reads its completions from `HccaDoneHead` and never looks at
  the register, so it was walking nothing. The queue's link also lives in
  each descriptor's next pointer at offset eight — the word after it is
  the buffer's last byte, and writing the link there handed the driver a
  list it could not follow.

  With both fixed the title's driver enumerates as far as reading the
  first eight bytes of the device descriptor, which it is answered
  correctly, and its interrupt routine is now called nineteen times a run
  rather than once.

- Timers fire. `KeSetTimer` arms one and the runtime queues its deferred
  procedure when it comes due, calling it the same way an interrupt
  routine's is; `KeCancelTimer` disarms one. A driver that polls its own
  state machine from a repeating timer had been initialising and never
  being heard from again.

  This is what the USB stack was waiting for. Its hub handler does nothing
  unless twenty frames have passed since it last ran — read straight out
  of the code, `cmp eax, 14h` against the frame number in the
  communication area — so it needed re-entering periodically, and only one
  interrupt was ever going to arrive. With timers firing, **the title
  resets and enables the root port**: the port went from `0x00000101`
  (connected) to `0x00000103` (connected and enabled) with thirteen writes
  where there had been two, and the driver is now executing its
  enumeration code.

- The graphics processor's vertical blank interrupt. `PCRTC_INTR_0` and its
  enable, and the `PMC_INTR_0` master status that surfaces them, are
  modelled rather than read as permanently clear, and a blank is raised
  about sixty times a second. The title enables it, and its routine is now
  called about 2,600 times in a run where it was never called at all. The
  captured frame is unchanged, so recorded digests keep their meaning.

- `exbawks run --gpu-methods` also reports the device registers the guest
  read most. That is what identified what Direct3D actually touches: the
  write-back cache flush bit it polls, the channel's `DMA_GET`, and a
  pattern register it reads back to order its writes.

- The root hub reports the four downstream ports the console's controller
  has, rather than one, and its port count is read-only as the hardware's
  is — an initialisation sweep could previously tell the hub it had no
  ports. Checked against xemu, which gives each Xbox OHCI four. The title's
  driver went from touching one port register to working all four.

- A transfer list is walked when its `HcControl` enable bit is set, not
  when the `HcCommandStatus` filled bit is: the filled bit is a hint that
  something new is on a list, which a driver reusing one need not set
  again, and requiring it meant never walking a list the driver had
  enabled.

- The gamepad's digital buttons are the sixteen-bit field the report
  layout defines, not a byte and a pad. The layout is confirmed against
  xemu's `XIDGamepadReport`, which matches this one field for field.

- The gamepad as a USB device, and the controller's descriptor walk: the
  device descriptor, configuration, and the controller's own descriptor
  that names its report size, plus `SET_ADDRESS`, `SET_CONFIGURATION`, and
  a state read; and an endpoint and transfer descriptor walker that runs
  the transfers a driver queues in guest memory, answering control
  transfers and returning reports on the interrupt endpoint. Every list
  walk is bounded, because the lists are guest data and a malformed one can
  be circular. Tested by driving a full enumeration — descriptor, address,
  configure, report — through descriptors built the way a driver builds
  them.

- Interrupt delivery. `KeInitializeInterrupt` records the guest's service
  routine and `KeConnectInterrupt` marks it live; when a modelled device
  raises an interrupt the runtime calls that routine on the guest processor
  between instructions, and restores everything it displaced when the
  routine returns. `KeInsertQueueDpc` queues the deferred procedure such a
  routine ends with, and the runtime calls that too. No programmable
  interrupt controller, interrupt descriptor table, or hypervisor injection
  is involved: the kernel is high-level emulated, so the routine is called
  like the ordinary function it is.

  This is the first interrupt this emulator has ever delivered. The title's
  USB driver now takes one, its service routine returns TRUE, its deferred
  procedure runs, and it writes the root port back — where before it
  initialised once and waited forever.

- An OHCI host controller and a synthetic Xbox gamepad (ADR 0019), in a new
  `exbawks-usb` crate: the register file, the root hub with its connect and
  reset handshake, the frame counter a driver times its debounce against,
  and the done queue. `exbawks run --gamepad` attaches a controller to the
  root port; the default remains no controller at all, so a run without it
  is the pure function of the image every recorded digest depends on.
  Input sources are `NoInput`, a deterministic `ScriptedInput` for tests,
  and — once the host side exists — a live controller, which by
  construction may not record a golden.

### Fixed

- A guest texture coordinate large enough to saturate the cast made the
  filtered sampler reach for a neighbouring texel at `i64::MAX`, panicking
  wherever overflow checks are on and reading an unrelated texel where they
  are not. Guest data must never panic the emulator, and the same input
  must not give a different pixel per build profile.
- A combiner stage's dot product was computed and then discarded instead of
  written to its destination, so a title shading through one read whatever
  the register held before. The sum is now suppressed instead, since a dot
  consumes both inputs.
- The combiner's primary and secondary colour registers are writable, and
  the final stage's alpha input carries an invert bit like every other.
- A texture's read window ran past its DMA object by the texture's own
  offset inside it, so a texture bound at a high offset could sample beyond
  the object it was bound to — which a mip chain, walking forward, reaches
  sooner.
- A vertex array's offset is checked against its object, so an array
  pointed outside cannot read another object's memory as geometry.
- The combiner census is capped like the method census; both halves of its
  key are guest-chosen, so it could grow until memory ran out.
- The mipmapped point-sampling filter codes are no longer treated as
  linear: they choose a level, they do not blend within it.
- `UBYTE_RGBA` vertex colours are four ascending bytes, not a packed
  `D3DCOLOR`; reading one as the other swapped red and blue.

### Added

- The interpreter executes x87 (ADR 0018): the register stack with its top
  in the status word, loads and stores across every memory format including
  the 80-bit one, integer loads and stores, the arithmetic and comparison
  families, exchanges, and the transcendentals. The stack holds `f64` and
  converts at the memory boundary, which is exact for the single and
  double precision a title actually stores. Three things unblock together
  — the write watchpoint can step past an `fstp`, the MMIO path can service
  a floating-point store to a device register, and the oracle tier follows
  a title through its geometry code. The interpreter's frontier on the
  retail image moves from x87 to SSE.

- `exbawks run --gpu-method-value` also reads back device registers when
  given an address in device space, so the display controller's state can
  be inspected the same way as a graphics method.

- A mip level is selected per triangle from how many texels it covers per
  pixel, instead of always reading the base level. The chain is walked as
  the hardware lays it out: levels end to end, halving but never
  vanishing, compressed levels padded out to whole four-by-four blocks so
  the smallest still cost one. The level is chosen once per triangle,
  which a steeply perspective one would want varying across it.

- Texture samples are filtered when the title's `SET_TEXTURE_FILTER` asks
  for it, blending the four texels around the sample. Dino Crisis 3 asks
  for linear magnification and trilinear minification on every texture;
  taking the nearest texel instead rendered its art as a mosaic. Mip
  levels are still not selected between — minification reads the base
  level — so a heavily minified texture still aliases.

- The texture addressing mode a title programs (`SET_TEXTURE_ADDRESS`) is
  obeyed: wrap, mirror, or clamp per axis. Every sample had been clamped,
  so a texture the title meant to tile held one edge texel across
  everything past its first tile — which is what turned this title's
  scene geometry into broad flat bands instead of detailed surfaces.

- Geometry at or behind the eye is dropped instead of drawn. A vertex with
  a non-positive `w` divides through to the far side of the screen and
  drags a wedge of geometry across the frame; 24,942 triangles per run of
  this title were being drawn that way. A vertex that cannot be projected
  also keeps its place in the vertex list now — dropping it renumbered
  every triangle an indexed primitive assembled after it.

- Fixed-function vertex lighting: emission, scene ambient, and one
  infinite light's ambient and diffuse terms, modulated by the vertex's
  own color. Dino Crisis 3 lights its title screen with a single
  directional light and had been drawing with an unlit white vertex color,
  which is where the scene's colour was going missing — the light carries
  the teal the screen is tinted with.

- Reflection-map texture coordinate generation, with the per-unit texture
  matrix that maps it into the image. Dino Crisis 3's title screen reflects
  an environment map off its geometry: it programs `SET_TEXGEN` mode
  `0x8512` on the second texture unit and supplies that unit no coordinate
  array at all, so without generating them the unit sampled a single texel
  across every primitive and shaded it flat.

- The register-combiner output word's two product destinations were read in
  the wrong order — the second product's destination comes first — and the
  dot-product and mux flags were read four to six bits too high. Checked
  against xemu's decoder. This title's output words name neither product
  destination, so its frame is unchanged; a title that used them would have
  been shaded from the wrong register.

- Texture format code `0x11` is `LU_IMAGE_R5G6B5`, not a linear
  `X8R8G8B8`; the linear `X8R8G8B8` code is `0x1E`. The swizzled
  `X8R8G8B8` code `0x07` is now decoded too. This title uses none of them,
  so nothing it draws changes.

- All four texture units are sampled, each with its own coordinate set,
  rather than only the first. This title's dominant draw — 44,757 of them
  on the captured surface alone — binds two units and multiplies one by the
  other, so a single unit could not reproduce what it puts on screen.

- The constant vertex attributes a title sets by method (`SET_VERTEX_DATA2F`,
  `SET_VERTEX_DATA4F`, `SET_VERTEX_DATA4UB`) are honoured. Dino Crisis 3
  sets its diffuse, specular, and second coordinate set that way for
  hundreds of thousands of primitives; every one of them previously read
  the hardware default instead.

- `exbawks run --gpu-combiner` prints the decoded combiner program of the
  last draw. It is what confirmed the method numbering against the retail
  stream: the title's own words decode to one stage passing the diffuse into
  a spare register and a final stage adding the specular to it.

- `exbawks run --watch-write <address>` reports every guest instruction that
  writes a chosen address. The page holding it is mapped read-only, so each
  write leaves the processor and is stepped on the interpreter, which turns
  "what sets this field?" from an afternoon of disassembly into a run. Its
  reach is bounded by what the interpreter can step: a title's `fstp` to a
  watched page ends the run, since only the x87 control subset is modelled.

- The blend factors, face culling, and the alpha test a title programs are
  now obeyed rather than assumed. Half of this title's blended draws ask for
  a destination factor of `ONE` — the additive passes that add light — and
  rendering them as source-alpha darkened exactly what should glow. Culling
  removes about a fifth of the shading work that was back faces painting
  over front ones. The winding convention is read from the title's own
  front-face and cull-face registers; if a scene ever appears inside out,
  that is the sign to check first.

### Fixed (graphics, from an adversarial review)

- Malformed pushbuffer data can no longer panic or hang the engine. A
  transform program start past the program's end now runs nothing instead of
  slicing out of range; the program and constant load pointers wrap instead
  of overflowing; a surface clip, whose two halves permit sixty-five thousand
  pixels a side, is bounded by what the hardware draws into and by the object
  holding it; the per-submission budget charges every dword the pusher reads
  rather than only headers, so a circular pushbuffer of long runs stops; the
  method census stops growing when a stream binds a fresh object handle per
  method; and the frame and texture captures refuse a size no display has.
- Texture samples and vertex-array fetches stay inside the object they were
  bound to, and a depth clear honours its object's limit the way the colour
  clear already did.
- `DXT3` and `DXT5` colour halves decode with four colours. They carry alpha
  separately, so reading them by `DXT1`'s rule turned a quarter of the texels
  of any block whose endpoints ascend into transparent black.
- A compressed or swizzled texture one texel on a side is sized by its format
  word rather than by the rectangle an earlier linear texture left behind.

- A private compatibility harness (`QA-002`): manifests under a directory
  named by `EXBAWKS_PRIVATE_FIXTURES` say which local image to run and which
  frame digest it should render, and `just goldens` runs them. The suite is
  ignored by default and inert without that variable, so a clean checkout
  still runs `cargo test` — no commercial data enters the repository, and a
  manifest whose image is missing is skipped rather than failed. Verified
  against a retail title: the harness ran it for three minutes and matched
  its recorded digest.

- Golden frames: `exbawks run --frame-digest` prints a captured frame's
  digest and `--expect-frame <digest>` fails the run when it differs, which
  turns a screenshot into a regression test. The digest covers the frame's
  shape as well as its pixels, and repeats exactly across runs of the same
  image — a title's frame came back byte-identical on consecutive runs, so
  the emulation is deterministic enough for the comparison to mean
  something. A title cannot be committed, so its digest belongs beside the
  private image in `fixtures/private/`, never in a test the repository runs.

- Depth testing and perspective-correct texturing. Fragments compare against
  the depth surface by the title's chosen function and update it when its
  depth mask allows, and `CLEAR_SURFACE` clears depth as well as color —
  without that clear every fragment is compared against whatever the memory
  last held, which rejected all but a twentieth of them. Texture coordinates
  now interpolate in the plane of the triangle rather than across the
  screen, so a textured surface seen at an angle no longer skews.

- Vertex arrays: a title that draws from arrays it uploaded once — indices
  through `ARRAY_ELEMENT16` and `ARRAY_ELEMENT32`, attributes at their own
  offsets and strides in a vertex context DMA — now assembles the same way
  inline data does.
- The fixed pipeline transforms by the composite matrix, which a title
  builds with the viewport already folded in: its third row carries the
  depth scale, the viewport register is left holding a sub-pixel offset, and
  each clip component is one matrix row dotted with the position. Two
  measurements settled it — the convention that puts most of the retail
  title's vertices on screen (64%, centred), and the reason none of them
  shaded a pixel afterwards: a mesh that carries no color attribute is
  white, not transparent black, and the alpha-zero pixels were all being
  skipped. Scene geometry now shades 1.3 billion pixels a run rather than
  none.

- GPU-M4: the vertex program. A title uploads 128-bit instructions and runs
  them once per vertex, two operations at a time — a vector one and a scalar
  one — over sixteen attribute registers, twelve temporaries, and a constant
  bank. `exbawks-gpu::execute` decodes and runs them; the engine feeds each
  vertex's attributes in, takes clip-space position, diffuse color, and the
  first texture coordinate set out, and divides by `w`. A program applies
  the viewport transform itself, from the scale and offset the title uploads
  as constants, which is why its results are already in pixels.
  `exbawks run --gpu-program` prints the instruction words, which is how the
  encoding was read off the retail title's own shaders.

- A capture keeps the most legible frame a run produced rather than
  whichever was current when it stopped. Each time the title finishes a
  frame — it starts drawing into the other buffer — the frame is scored by
  how much it varies, and the best is kept: a fade passes through flat black
  and flat white, and neither is the picture.

- GPU-M3: texture sampling and blending — **the retail title's screen now
  renders**. The engine follows the first texture unit's state (offset,
  format, pitch, image rectangle, and the context DMA its offset is
  relative to), samples linear and swizzled `A8R8G8B8` as well as `DXT1`,
  `DXT3`, and `DXT5` blocks, modulates the texel by the interpolated vertex
  color, and blends by source alpha when the title enables blending — which
  is what stops its transparent art from painting black over the frame.
  `exbawks run --dump-texture <file.png>` writes the most recently sampled
  texture, which is how the retail atlas was proven to decode; `--gpu-methods`
  additionally reports which color surfaces received drawing, and
  `--screenshot-address` captures a chosen surface.
- A capture now chooses the frame by looking: of the display-sized surfaces
  the engine drew into, it takes the one carrying picture, because which
  buffer is on screen depends on where the title sits in its rotation.
- The guest time-stamp counter advances at the console's 733 MHz clock
  rather than 10 MHz, so `rdtsc`-paced animation runs at its own speed.
- An event wait nothing can signal now completes instead of reporting a
  timeout. A title paces frames on events a device interrupt would set;
  with no interrupts and synchronous device work, the wait is satisfied in
  fact, and a timeout only made the caller spin on it forever.

- GPU-M2: a triangle rasterizer (`exbawks-gpu::fill_triangle`) and the
  vertex assembly that feeds it. Primitives arrive through `SET_BEGIN_END`
  and `INLINE_ARRAY`, the declared attribute layout gives each vertex its
  stride, packed `D3DCOLOR` and float colors interpolate barycentrically,
  and triangles, strips, fans, and quads all assemble. A capture now reads
  the surface the engine drew into before the current one — with double
  buffering that is the finished frame, which the encoder's programmed
  address no longer tracks once a title flips on its own.

  The retail title's pre-transformed geometry (positions already in pixels,
  far outside any clip volume) draws; everything a vertex program transforms
  does not, and is counted as skipped rather than drawn wrong. A vertex
  stage and texturing are what stand between this and a recognizable frame.

- GPU-M1: render-target state and `CLEAR_SURFACE`. The pushbuffer engine
  tracks the color surface a channel programs — its context DMA, offset,
  pitch, and clip — and a color clear fills the clip-intersected rectangle
  in guest memory with the clear value, through a bulk fill the emulator
  backs with one write per scanline. `exbawks run --gpu-methods <n>` reports
  the most-submitted methods, which is how the retail title's stream was
  read: it binds a Kelvin object, uploads matrices, sets vertex formats, and
  draws through inline arrays and 16-bit element indices.
- **The graphics fence now lands where Direct3D reads it.** Its pushbuffer
  wait polls a counter in memory and spins until the GPU catches up; the
  fence writes were going to the report object (all of RAM, offset zero)
  instead of the semaphore object, so the wait never completed and the title
  stalled after four submissions. The object each method binds settles the
  numbering: `0x01A4` binds the 32-byte block the wait polls, `0x01A0` binds
  the whole of RAM. With fences landing there, a two-minute run goes from 4
  pushbuffer submissions to 72,853, with 12,601 surface clears — the title
  renders continuously.

- Frame capture: `exbawks run --screenshot <file.png>` writes the frame the
  title programmed on the video encoder. `AvSetDisplayMode` now records the
  mode through a kernel service, `Emulator::capture_frame` reads the linear
  32-bit surface through the cached physical window (ADR 0010), and
  `exbawks-debug` encodes it with a dependency-free PNG writer (deflate
  stored blocks, deterministic byte-for-byte so goldens are stable).
- `exbawks run --peek <addresses>` prints guest dwords after a stop, which is
  how a title's own state machine gets read without a debugger.
- Dispatcher-object waits (`KeWaitForSingleObject`, `KeSetEvent`), mutants
  (`NtCreateMutant`, `NtReleaseMutant`, with recursive ownership and a
  contended wait that parks), thread control (`NtResumeThread`,
  `NtSuspendThread`), and `NtWaitForMultipleObjectsEx`.
- A real global descriptor table for the guest. The boot state loads segment
  registers with cached descriptors, which executes fine until guest code
  performs an actual segment load — BINK's CPUID probe ends with `pop es`,
  which raised `#GP` against the empty table. The table now lives on a
  reserved low page with null, code, data, and `fs` entries, the last
  carrying the running thread's KPCR base so a reload keeps it.

- GPU-M0: the NV2A pushbuffer engine (`exbawks-gpu::PushbufferEngine`).
  When the guest advances a channel's `DMA_PUT`, the engine replays the
  hardware DMA pusher over guest RAM: method headers (increasing and
  non-increasing), jumps, calls, and returns; per-method accounting; object
  binding; and `RAMHT` resolution of DMA objects from instance memory so
  the back-end semaphore release writes fence values into guest memory —
  the readbacks Direct3D waits on. Pure logic over a physical-memory trait,
  unit-tested with a flat fake (walks, RAMHT-resolved semaphore release,
  call/return, bad-word abandonment); the emulator adapts it through the
  cached window and consumes submissions while the vCPU is parked.
- The Direct3D bring-up burndown continued, each wall named by the retail
  image: interrupt-status registers are write-1-to-clear, so they read
  clear rather than latching the guest's ACK (phantom `NV_PGRAPH_INTR`
  bits sent D3D looping through its exception handler into a crash); the
  PFIFO status family answers as idle hardware (empty marks set, DMA
  pusher enabled and drained) so the FIFO drain loop exits; the `Av*`
  family is real (`AvSendTVEncoderOption` answers a synthetic NTSC
  profile, `AvSetDisplayMode` logs and accepts the mode — the retail image
  sets its display mode); `KeDisconnectInterrupt` reports TRUE;
  `NtFreeVirtualMemory` validates and acknowledges (the region leaks until
  MEM-006); and the X:/Y:/Z: cache drive letters mount as writable
  per-title scratch under the HDD directory — the title was crashing on an
  unchecked `fopen(Z:\DATA\Ini.itk)` failure. The retail image now sets
  its display mode and loads title-screen assets (`us_font.xtx`,
  `BG.itk`, `pack_00.itk`) from the disc.
- The cached physical window is real (ADR 0010): virtual
  `[0x8000_0000, 0x8000_0000 + ram)` aliases physical `[0, ram)`, kernel
  blocks are physical allocations whose virtual address is the window
  identity `0x8000_0000 | PA`, and the loader reserves kernel-owned low
  physical memory up front — page 0 as the scratch/zero page (Direct3D
  primes cache flushes by writing through the window base) and physical
  `0x10000` for the synthetic kernel image at its architectural address.
  `MmGetPhysicalAddress` now performs a real page-table walk, so physical
  addresses are truthful for every mapped virtual address (the PRAMIN
  alias and GPU-visible pointers inherit the correctness). The ADR 0010
  stack-guard fault property is traded away — under the window every
  physical page is mapped; guard pages remain as burned spacing. An
  adversarial review caught the refactor silently killing the ADR 0015
  soft-reboot restore (window addresses are always mapped, so the old
  remap-then-write gate failed and launch data was lost while the pointer
  test still passed): persisted window regions now reserve their physical
  pages before the fresh boot allocates and restore by writing straight
  through the window, a content-integrity relaunch test pins the path,
  kernel blocks are scrubbed on allocation (the guest can dirty any page
  through the window first), reservation bounds clamp to real RAM, and
  emulated RAM is capped at the 128 MiB console ceiling so the window can
  never reach device space.
- First pushbuffer contact: writes to the NV2A `USER` channel `DMA_PUT`
  latch `DMA_GET` to the submitted value (an infinitely fast GPU), with the
  submission logged for the coming graphics frontend, and the PFB trigger
  register (`0xFD10_0410`) self-clears. The retail image programs the GPU
  through PFIFO/PFB/PCRTC, submits its first pushbuffer, and proceeds into
  Direct3D's post-submission path — the frontier where real command-stream
  consumption (the `GraphicsFrontend`) begins.
- The Direct3D-init burndown on the hypervisor tier. The GPU instance
  claim now backs the NV2A `PRAMIN` window: the engine aliases the claimed
  region into the partition (and the interpreter's device view redirects
  `PRAMIN` to it), so instance-memory traffic is plain RAM instead of
  per-access exits. Both guest clocks advance from wall time on the WHP
  tier (`KeTickCount` and the `KeQuerySystemTime` TSC — the interpreter
  that normally ticks the TSC only runs for MMIO steps here, so a
  time-based startup would otherwise wait on a frozen clock). Port I/O
  exits are serviced against a latching port model (the VGA CRTC path
  D3D drives), and `HalReadWritePCISpace` serves a synthetic NV2A PCI
  configuration space (identity `0x02A0_10DE`, enabled command register,
  register/framebuffer BARs).
- Preemptive time slices on the hypervisor tier (ADR 0017): the cancel
  pump rotates ready guest threads round-robin every few cancellations, so
  a compute-only thread (DirectSound's sample mixer) can no longer starve
  the rest of the guest — the flaw the cooperative-only ADR 0011 scheduler
  hit once real work ran deep. Blocking waits became real to match:
  `NtWaitForSingleObject`/`NtWaitForSingleObjectEx` park the caller on an
  event or thread handle (a new `Waiting` thread state), `NtSetEvent` and
  thread exit wake the parked waiters, and a wait with no other runnable
  thread reports `STATUS_TIMEOUT` rather than deadlocking. The interpreter
  tier stays purely cooperative and deterministic.
- The DSP mailbox refined to write-through semantics: writes land in RAM
  and only the most recently written cell (the polled FIFO head) reads as
  consumed, so setup data sharing the comm page survives. DirectSound's
  initialization now completes natively, and Direct3D begins:
  `MmClaimGpuInstanceMemory` claims the NV2A instance area from the
  contiguous allocator. The retail image proceeds into distributed
  D3D/game-side initialization on the hypervisor tier.
- The audio-path burndown over the M2 device model, each piece demanded by
  the retail image's DirectSound init in order: ready-bit overrides for the
  APU GP-DSP status and the AC'97 `GLOB_STA` codec-ready bits (a zero read
  spins or fails `DirectSoundCreate`, whose HRESULT the image never checks);
  self-clearing AC'97 channel-control registers (latching a reset bit spun
  forever — real hardware clears it instantly); and instant-consumer DSP
  command mailboxes — when the guest programs a GP comm region's physical
  base, the engine unmaps that region's mailbox page from the partition and
  consumes writes the moment they land, breaking the "wait for the DSP to
  drain the FIFO" spin. The cancel pump now doubles as a sampling profiler
  (trace-level RIP/RBX per cancellation), which is how each native spin was
  located. New exports: `HalGetInterruptVector`, `KeInitializeInterrupt`,
  `KeConnectInterrupt` (connected, never fired — devices are HLE), and
  `MmLockUnlockBufferPages` (benign; HLE pages never move).
- WHP-M2: device MMIO dispatch. Hardware register blocks
  (`0xFD00_0000..0xFF00_0000`: NV2A GPU, MCPX APU, AC'97) stay unmapped in
  the partition; a guest access exits and the engine executes exactly one
  instruction on the interpreter over an `MmioView` that overlays the device
  model onto RAM — addressing forms, read-modify-write, and flags come from
  the oracle, not a hand decoder. The stub device model latches writes for
  readback (device init programs base addresses and rereads them; zero
  reads corrupt pointers), returns zero for unwritten registers, answers
  all-ones for observed hardware-set ready bits (the APU GP-DSP status the
  retail image polls), and counts accesses per region so a stalled boot
  names the device it waits on. An end-to-end test drives a native guest's
  APU write and readback through the stub to a clean exit. New exports the
  deeper run demanded: `KeStallExecutionProcessor` (benign no-op),
  `MmQueryAllocationSize`, and `NtSetInformationFile` (position, allocation,
  and end-of-file classes over new file-position/length services). The
  retail image now initializes DirectSound far enough to create its
  title-data files, probe the config partition, and load `dsstdfx.bin` (the
  DSP effects image) from the disc.
- WHP-M1: the retail image runs natively on the hypervisor tier (ADR 0013).
  Guest physical RAM moved into a page-aligned, address-stable allocation
  (`exbawks-platform::AlignedBuffer`) that the partition maps directly — the
  software MMU and the hypervisor read and write the same bytes, so the two
  tiers stay coherent by construction. `Emulator::run_whp` mirrors the
  software page table into guest-physical space (mapping epochs trigger
  resyncs), boots the vCPU from `CpuState` (SSE enabled via
  `CR4.OSFXSR/OSXMMEXCPT`), services kernel gates through fetch-fault exits
  with the existing gate-by-EIP dispatch (registers sync around each call,
  including the per-thread `fs` base, so the cooperative scheduler works
  unchanged), honors thread null/sentinel exits and the soft-reboot
  relaunch, maps intercepted guest exceptions to typed stops, and advances
  the virtual clock from a cross-thread cancel pump. `exbawks run --engine
  whp` selects the tier; an end-to-end test boots the synthetic title
  natively through two gate dispatches to a clean exit. New exports the
  deeper run demanded: the `Ex*` pool family (`ExAllocatePool`,
  `ExAllocatePoolWithTag`, `ExFreePool`, `ExQueryPoolBlockSize` over
  size-tracked pool blocks) and the IRQL family (`KfRaiseIrql`,
  `KfLowerIrql`, `KeGetCurrentIrql`, `KeRaiseIrqlToDpcLevel` maintaining
  `KPCR.Irql`). The retail image now runs natively past its SSE game code
  and D3D/DSOUND initialization and stops at its first audio-hardware MMIO
  touch (`0xFE80_0200`) — the device-emulation frontier for the next
  milestone.

### Performance

- Guest writes are no longer O(page table): `bump_physical_generation`
  synchronized every descriptor's embedded generation stamp by walking all
  2^20 entries on **every guest write** (~3 ms each), grinding write-heavy
  guest phases to a crawl — diagnosed by stack-sampling a "frozen" run that
  was 100%-CPU inside the walk. The per-physical-page generation array is
  the authoritative store (ADR 0005): dependencies are now captured from it
  as well as revalidated against it, and the walk is gone. The interpreter
  runs ~1000× faster in write-heavy phases; the retail image completes its
  **entire kernel-facing initialization in under a second** and stops at its
  first SSE instruction (`movss`) — the CPU-tier frontier the WHP pivot
  (ADR 0013) exists for.

### Fixed

- **Direct3D device creation on the retail image.** `AvSendTVEncoderOption`'s
  capability query (option 6) answered `1`, a value naming no video standard.
  Direct3D uses that word to pick a display-mode group — video standard in
  bits 8..15, AV pack class in bits 0..7, and a capability bit every SDTV
  mode entry carries — so its mode search matched nothing and returned
  `E_FAIL`; `Direct3D_CreateDevice` then ran its own teardown, leaving the
  device global NULL while the title used it anyway. The query now answers
  `AV_STANDARD_NTSC_M` (`0x0040_0100`). The retail title now creates its
  device, sets its display mode, and drives millions of pushbuffer methods.
- The scheduler reported a guest exit when the last *ready* thread ended
  while others were parked on objects nothing could signal. Those waits are
  satisfied in fact — the emulated devices raise no interrupts and complete
  their work synchronously — so they are released and the run continues.


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
