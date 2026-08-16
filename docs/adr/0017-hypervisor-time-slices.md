# 0017. Preemptive time slices on the hypervisor tier

Date: 2026-08-16

Status: Accepted

## Context

ADR 0011 chose a cooperative single-processor scheduler: guest threads
switch only at kernel dispatch points, where the emulator already owns the
thread's full register state. That contract held through the interpreter
tier and the hypervisor tier's kernel-facing boot.

Running the retail image deeper broke it: DirectSound's mixer thread loops
forever doing pure computation — per-sample mixing with no kernel call in
its steady state. On hardware it is preempted by interrupts; under a
cooperative scheduler it monopolizes the processor and the main thread
never runs again. Any guest thread that computes without calling the
kernel starves every other thread.

The hypervisor tier already has a controlled interruption point: the
cancel pump kicks the virtual processor out of `WHvRunVirtualProcessor`
every millisecond to advance the virtual clock, and a cancellation exit
carries the full architectural state.

## Decision

On the hypervisor tier, cancellation exits are scheduling points: every
few cancellations the engine saves the active thread's registers, rotates
to the next ready guest thread round-robin, and restores its registers —
a time slice of a few milliseconds.

Kernel dispatch points keep their ADR 0011 role; rotation is in addition,
not a replacement. The interpreter tier stays purely cooperative — it is
the deterministic oracle, and determinism outranks liveness there.

## Consequences

- Compute-only guest threads no longer starve the system; the mixer
  thread and the main thread interleave as they would under interrupts.
- Thread switching is no longer deterministic on the hypervisor tier.
  That was already true of its clock (wall-time ticks); golden runs stay
  on the interpreter tier.
- Rotation costs one register read and one register write per slice, a
  few microseconds every few milliseconds.
- A guest thread's critical sections guarded only by IRQL bookkeeping
  could now interleave differently than under ADR 0011. The Xbox kernel's
  own primitives (critical sections, events) remain honored at dispatch
  points; if a title exhibits a rotation-timing bug, the slice length is
  the tuning knob.
