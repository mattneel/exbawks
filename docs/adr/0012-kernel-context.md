# ADR 0012: Kernel Services Context

- Status: accepted
- Date: 2026-08-15

## Context

`KernelCallContext` exposes only mutable CPU state and checked guest memory,
so no export can allocate guest memory, create a thread, read a clock, or
schedule work. Thread creation (`PsCreateSystemThreadEx`) needs all of that
now, and the M3 export families need more. Widening the context breaks the
public `exbawks-kernel` contract, which requires this ADR first.

## Decision

`KernelCallContext` gains one field: `services: &mut dyn KernelServices`.

`KernelServices` is a trait defined in `exbawks-kernel` and implemented by
`exbawks-core`. Its surface grows one narrow, typed request/response method
per capability; exports never receive host state, allocator internals, or
the emulator object. The initial surface:

- `create_thread(&mut self, request: ThreadCreateRequest)
  -> Result<ThreadCreated, KernelServiceError>` — allocates the guest stack,
  KPCR/KTHREAD pages, and TLS block per ADR 0010, registers the thread per
  ADR 0011, and returns the guest-visible handle, thread identifier, and
  KTHREAD address.
- `exit_current_thread(&mut self, status: u32)` — records the pending
  termination the run loop applies after the export returns (ADR 0011's
  no-direct-switch rule).

Later work extends the trait the same way: virtual-clock queries, dispatcher
object operations, guest virtual-memory calls, and the pending-guest-call
queue for DPC/APC/ISR invocation. Each addition names the boot-plan task
that introduces it.

Service methods that allocate operate on the address space directly (the
implementation holds its own handle), never through the caller-visible
`GuestMemory` borrow, so the borrow split between `cpu`, `memory`, and
`services` stays trivially sound.

Tests and tools that need a context without an emulator use the
`UnsupportedServices` implementation exported by `exbawks-kernel`, whose
methods return typed unsupported errors.

## Consequences

Every existing `KernelCallContext` construction site gains a `services`
field; the change is mechanical and breaks no behavior for exports that
ignore it.

Export implementations stay pure guest-facing logic: argument parsing,
service calls, out-parameter writes, and NT status codes, which keeps them
portable and unit-testable against `UnsupportedServices` plus fakes.

The trait boundary is the seam future subsystems mock in kernel-crate unit
tests, and the place where the pending-action pattern (ADR 0011) is
enforced by construction rather than by convention.
