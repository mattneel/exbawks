# ADR 0021: Real wait semantics

## Status

Accepted. Amends ADR 0011 (cooperative scheduler) and ADR 0017 (preemptive
time slices).

## Context

The synchronization layer was built one wall at a time, and a structural
review (2026-08-19) found that what accumulated is not a model of the
console's kernel but a collection of ways to keep one title moving:

- A wait that no other *runnable* thread could satisfy completed with
  fabricated success, on the stated premise that "nothing in the emulator
  raises an interrupt." The hypervisor tier has delivered vblank and USB
  interrupts for some time, and interrupt routines are guest code that
  signals dispatcher objects, so the premise is false and the fabrication
  masks real waits.
- `RtlEnterCriticalSection` acquired a section whose recorded owner was a
  live parked thread, and a contended section broke on wake: the release
  zeroed ownership and woke *every* waiter, none of which re-acquired. Two
  threads could proceed inside one section — the exact failure a heap
  guard exists to prevent, and the likeliest cause of the intermittent
  free-list corruption at `0x001AE56C`.
- Every wait export ignored its `Timeout` argument, so a zero-timeout poll
  — NT's standard non-blocking idiom — parked forever.
- `NtWaitForMultipleObjectsEx` recorded each unsatisfied handle into the
  single `PendingAction` slot, parking the thread on whichever handle came
  last, and could park a thread that had already completed its wait.
- Event handles were allocated from the live map's size, so closing one
  event made the next creation reuse a still-open handle.
- When the last runnable thread exited, every parked thread was woken
  regardless of what it waited on.

Each shortcut was individually defensible when the scheduler had no
interrupts, no timers, and effectively one thread. None survives the
current runtime, where the retail title runs six threads, arms timers, and
takes vblank and USB interrupts.

## Decision

Waits become real. The pieces:

**A wait is a block, not a handle.** A parked thread records the full
wait: the keys it waits on (handles or guest object addresses), whether it
needs any or all of them, an optional deadline in virtual milliseconds,
and the status a timeout should return. The one-slot `PendingAction` keeps
its role — a kernel export still cannot block mid-call (ADR 0011) — but
what it records is the whole block, and recording a second wait in one
call replaces the first rather than stacking.

**Wakes go through one arbiter.** Signaling a key finds the threads whose
blocks contain it. A wait-any block is satisfied immediately: the woken
thread's saved `EAX` is set to `STATUS_WAIT_0` plus the key's index in
*its* wait, which is what makes multi-object waits report the right
winner. A wait-all block is re-checked against all its keys and satisfied
only when every one reports signaled. How many threads one signal wakes is
the object's decision, not the caller's:

- an auto-reset event wakes exactly one waiter and is consumed by that
  wake — whether it was signaled by handle or by guest address (the
  dispatcher header's type byte says which semantics apply);
- a notification event and a terminated thread wake every waiter and stay
  signaled;
- a released mutant wakes exactly one waiter and transfers ownership to it
  in the same step;
- a released critical section hands ownership to exactly one waiter: the
  release stamps that thread's KTHREAD into `OwningThread` with a
  recursion count of one before waking it, so the woken thread resumes
  already holding the section. Nothing else may take it in between —
  in the cooperative model nothing else runs in between.

**Timeouts are honored.** A wait export reads its `Timeout` argument: a
null pointer waits forever, zero polls (an unsatisfied poll returns
`STATUS_TIMEOUT` without parking), and a negative value is a relative
interval converted to virtual milliseconds and recorded as a deadline.
Timer advancement sweeps deadlines and wakes expired waiters with the
block's timeout status. `KeDelayExecutionThread` is a block with no keys
and a deadline, whose "timeout" status is success — a sleep. An absolute
(positive) timeout is treated as infinite with a trace note; no title on
the current path uses one, and pretending to convert it against a clock
the runtime does not model would be another fabrication.

**Idle is a state, not a lie.** When no thread is runnable, the runtime
does not fabricate a completion; it idles. On the hypervisor tier the idle
loop advances the clocks, raises due vblanks, delivers interrupts and
deferred procedures — guest code that signals objects and readies waiters
— and resumes the first thread that becomes runnable. On the interpreter
tier, which has no device pump, idle advances virtual time directly to the
earliest deadline or timer. If no wake source exists at all — no deadline,
no armed timer, no connected interrupt — the guest has genuinely
deadlocked, and the run stops with `StopReason::GuestDeadlock` and the
thread table, because reporting a deadlock is diagnosis and papering over
one is how the last three months of this layer happened. The hypervisor
idle loop also carries a wall-clock ceiling against a wake source that
never fires.

**Waits at interrupt level are refused.** An interrupt or DPC routine runs
as a borrowed frame on whichever thread was interrupted; parking that
thread would park an unrelated wait. A wait recorded while an interrupt
frame is live is dropped with a warning, which mirrors the real kernel's
rule that waiting at raised IRQL is illegal.

**Handles come from cursors.** Event and mutant handles are allocated
monotonically, never from the live map's size, so a closed handle's value
is not reissued while its neighbors live.

## Consequences

`KeWaitForSingleObject`, `NtWaitForSingleObject(Ex)`,
`NtWaitForMultipleObjectsEx`, `KeDelayExecutionThread`,
`RtlEnterCriticalSection`, and `RtlLeaveCriticalSection` change behavior;
the fabricated-success paths and the blanket wake are deleted. Runs that
depended on fabrication — every run of the retail title to date — will
take different paths past their waits; recorded digests are expected to
survive only where the waits were genuinely satisfied, and any that do not
were resting on the fabrication.

The scheduler's thread state loses `Copy` (a wait block owns its keys),
and the wait services gain timeout parameters and a multi-object form.

What this does not fix, recorded so nobody mistakes the scope: the
interpreter tier still cannot run the retail title (no SSE), the device
pump is still fused to the hypervisor tier, and floating-point state still
bleeds across hypervisor thread switches — those are the following work
items, not this one.
