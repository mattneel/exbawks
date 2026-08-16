# ADR 0011: Cooperative Deterministic Scheduler

- Status: accepted
- Date: 2026-08-15

## Context

Retail XAPI startup calls `PsCreateSystemThreadEx` as its first kernel call —
before heap initialization — so the main guest thread is kernel-created and
nothing boots without thread support. Golden testing requires byte-identical
traces across runs, so scheduling must be deterministic. The dispatcher ABI
(ADR 0006) and the interpreter (ADR 0008) both execute against one active
`CpuState`.

## Decision

One host thread runs every guest thread. The emulator owns a thread table of
`GuestThread` records (guest `CpuState`, stack range, KPCR and KTHREAD
addresses, and a Ready/Running/Suspended/Terminated state); the embedded
`CpuState` remains the single active context, and a switch saves it into the
outgoing record and loads the incoming record.

Guest threads switch only at kernel dispatch points:

- Thread termination switches to the next ready thread. `PsCreateSystemThreadEx`
  does not switch; the creator continues, as on hardware.
- Waits, delays, and yields become switch points when the dispatcher-object
  work lands; timer expiry runs on the virtual clock at those same points.
- A fixed block-count quantum becomes a switch point when more than one
  thread is ready; until then threads run to their next kernel call.

The ready queue is FIFO in thread-creation order. No host-time input ever
reaches a scheduling decision.

A thread start routine returns to the reserved sentinel address
`0xFFBF_FFF4`, which sits in the reserved region above the kernel gate range
so `gate_ordinal` never resolves it to an export. The run loop treats
execution reaching the sentinel as an implicit `PsTerminateSystemThread`
whose exit status is the routine's EAX return value.

Kernel exports never switch directly: a service call records a pending
scheduling action, and the run loop applies it after the export returns, so
the export's view of `CpuState` stays coherent for the full call.

Terminating the last runnable thread stops emulation with
`StopReason::GuestExit` carrying the exit status.

Rejected: one host thread per guest thread. Preemption would require
signal-safe interruption of generated code and would destroy trace
determinism; the repository's whole test strategy assumes reproducible runs.

## Consequences

Thread creation must allocate guest stacks, KPCR/KTHREAD pages, and TLS
blocks, which lands with the kernel services context (ADR 0012) and the
ADR 0010 address map.

Long-running guest loops between kernel calls cannot starve correctness —
only responsiveness — because everything is one host thread; the block-count
quantum addresses fairness when multi-threaded titles need it.

Blocking kernel waits must never block the host thread; they park the guest
thread and switch, which shapes the dispatcher-object implementation.

Deterministic FIFO wake order diverges from hardware priority boosting;
priority-sensitive titles may need the priority fields later, recorded as an
extension point rather than implemented now.
