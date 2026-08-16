# ADR 0015: Soft-Reboot Title Relaunch

- Status: accepted
- Date: 2026-08-16

## Context

After the Dino Crisis 3 image completes its disc and HDD-partition probe it
relaunches itself. Its startup calls `XLaunchNewImage(NULL, launchData)`, whose
kernel-side path allocates a contiguous `LAUNCH_DATA_PAGE`, sets the kernel
`LaunchDataPage` variable (data ordinal 164) to point at it, marks it with
`MmPersistContiguousMemory`, and calls `HalReturnToFirmware(ReturnFirmwareQuickReboot)`.
The observed page carries `dwLaunchDataType = 1`, `dwTitleId = 0x43430003`
(the title's own id), and an empty `szLaunchPath` — a self-relaunch. On the
second boot the title reads the launch data and takes its post-relaunch path
instead of relaunching again.

Today `HalReturnToFirmware` maps every routine to `StopReason::GuestExit`, so
the run simply ends. Reaching the title screen requires the emulator to honor
a soft reboot: preserve the persisted launch data across a machine reset and
re-run the image, exactly as the console's firmware does.

This introduces a relaunch loop, launch-data preservation across a reset, and
new public surface (`StopReason`, a `KernelServices` method), so it needs an
ADR.

## Decision

Model the console's soft reboot in the emulator, driven by the guest.

### Reboot signal

Add `StopReason::Reboot { routine: u32 }`. `HalReturnToFirmware` sets it for
the reboot routines (`ReturnFirmwareReboot` = 1, `ReturnFirmwareQuickReboot`
= 2) and keeps `GuestExit` for halt/fatal (0, 4). The reboot is a controlled
stop like any other; the composition root decides what it means.

### Persist tracking

`MmPersistContiguousMemory` becomes a real export that records the persisted
region through a new `KernelServices::persist_memory(base, size)`. The
implementation (`ThreadManager`) keeps the list of persisted `(base, size)`
regions — the memory the guest asked the firmware to keep across the reboot.

### Relaunch in the composition root

`Emulator::run` wraps the block loop. On `StopReason::Reboot`:

- If the `LaunchDataPage` variable (ordinal 164 cell) is null, there is no
  relaunch target (a plain reboot to a dashboard the emulator does not host);
  `run` returns the `Reboot { routine }` stop unchanged.
- Otherwise snapshot the bytes of every persisted region and the
  `LaunchDataPage` pointer value, `reset()` the machine, reload the same image
  (`LoadedImage::bytes`; the empty launch path means relaunch the current
  title), then restore each persisted region at its original guest address
  with its saved bytes — advancing the kernel bump cursor past it so the new
  boot never re-allocates over it — and write the saved pointer back into the
  freshly rebuilt `LaunchDataPage` cell. Continue running.

A relaunch that persists byte-identical launch data to the previous one is a
reboot loop — the title cannot satisfy some condition the emulator does not
model (a display mode, for one) — so `run` stops with the `Reboot` reason
instead of relaunching again. A counter backstops the case where the launch
data keeps changing; on either limit `run` returns the `Reboot` stop so the
caller sees a controlled reason rather than a hang.

The kernel-variables layout is deterministic for one image, so the
`LaunchDataPage` cell lands at the same guest address on every boot; the
`ThreadManager` records that address when it builds the variables and exposes
it to the relaunch path.

### Same-title only, for now

Only an empty `szLaunchPath` (self-relaunch) is handled. A non-empty launch
path (launching a *different* XBE, e.g. a dashboard or a sequel disc) resolves
through the disc mount and is later work; until then such a reboot returns
`GuestExit`.

## Consequences

`run` no longer returns on the guest's own quick-reboot; it transparently
carries the title across the soft reboot, which is what a title expects. A
caller that wants to observe the reboot can still see it through the trace
(`Stop`/relaunch records).

The persisted-memory model is intentionally minimal: regions are re-mapped
with fresh physical pages and their saved contents, not aliased to surviving
physical pages, because the real physical allocator and its cross-reset
survival are still `MEM-006`/`MEM-007`. This is enough for launch-data
preservation and nothing more.

Because only self-relaunch with launch-data preservation is modeled, a title
that reboots for another reason (a genuine dashboard return, a different-image
launch) ends the run rather than looping — a safe, visible stop.
