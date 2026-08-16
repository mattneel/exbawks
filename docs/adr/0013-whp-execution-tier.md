# ADR 0013: WHP Primary Execution Tier

- Status: accepted
- Date: 2026-08-15

## Context

The guest is a 733 MHz Coppermine running 32-bit protected mode with SSE1.
Any host that can create a Windows Hypervisor Platform (WHP) partition
executes that instruction stream natively under VT-x/AMD-V with unrestricted
guest support. The interpreter (ADR 0008) already runs the retail boot path
correctly, but it is slow, and the planned hand-written JIT (ADRs 0006/0009,
JIT-004..010) is a large, bug-prone effort that reimplements what the host
CPU already does.

ADR 0008 established a tiered execution model precisely so the engine can be
swapped behind a stable contract. Kernel exports depend only on register
state and checked guest memory, not on how instructions execute, so the
execution engine can change without touching the HLE.

## Decision

WHP becomes the primary execution tier. A new `exbawks-whp` crate runs the
guest on one WHP virtual processor; the interpreter is retained as the
deterministic oracle and golden-test tier.

**Tier roles.**

- WHP (Windows-only, native speed): the default runtime engine. Deletes the
  entire CPU-emulation surface — memory-operand lowering, control flow, x87,
  MMX, SSE1, self-modifying-code invalidation — because the host CPU handles
  it. The JIT track (JIT-004..010) is dropped.
- Interpreter (portable, deterministic): retained as tier 0. Golden frame and
  trace tests run here, so determinism is a property of the tier that produces
  goldens, not a property WHP must guarantee. It remains the Linux
  `portable-logic` CI engine and the reference for any WHP behavior question.

**The seam is narrow.** The WHP surface is small: create a partition, set
processor count and extended VM exits, map guest physical address ranges,
create one vCPU, and run a dispatch loop over a handful of exit reasons
(memory access, CPUID, I/O port, halt, cancel). Everything else is the
device model and the HLE, which hang off exits. `exbawks-whp` exposes the
vCPU register set and the mapped guest memory behind the same abstractions
the kernel HLE already consumes.

**Device layer stays HLE.** This ADR decides the CPU engine, not the device
model. Consistent with the project charter, the kernel and graphics stay
high-level: kernel calls are trapped at the import-thunk gate region (mapped
not-present so the fetch faults out with the ordinal encoded in the guest
physical address) and dispatched to the existing Rust exports; graphics is
NV2A pushbuffer HLE. The existing kernel HLE — the ordinal table, the
`KernelServices` contract, thread creation, the object and file work to
come — is engine-independent and carries over unchanged.

**Memory map is exit economics** (mapping choices are the real WHP design).

- RAM backed at guest physical address 0, mapped read-write-execute.
- The GPU aperture aliases the same host RAM pages at `0xF000_0000` so
  write-combined and streaming stores there never exit.
- `0xFD00_0000` is left unmapped so NV2A register traffic faults out; the
  only hot register on the geometry path is the FIFO kick, because
  pushbuffers live in guest RAM and submit exit-free.
- The dirty-page bitmap query is the texture-cache invalidation signal.
- The kernel gate region stays not-present so thunk calls trap.

**Determinism.** WHP execution is not reproducible (guest RDTSC runs at host
frequency with no reliable intercept). This is acceptable because goldens run
on the interpreter tier. A per-title compatibility note records any title
whose pacing depends on TSC; most pace off vblank and hardware timers.

## Consequences

The hand-written JIT is cancelled; ADRs 0006 and 0009 become historical. The
codegen contract and physical-page generation tracking are no longer on the
critical path (generation tracking stays only as the interpreter tier's SMC
story).

`exbawks-whp` is a new, Windows-only, unsafe FFI surface over
`WinHvPlatform.dll` and `WinHvEmulation.dll`. It follows the unsafe policy:
the smallest possible modules, `SAFETY:` comments stating pointer lifetime,
alignment, mapped range, and thread requirement, and RAII wrappers for the
partition, vCPU, and mapped ranges. It does not build on non-Windows hosts;
the crate is absent from the portable-logic job.

Kernel HLE work is preserved. The export bodies read and write a vCPU context
and mapped guest memory through the same shapes they use today; only the
concrete backing changes.

The instruction emulator (`WinHvEmulation`) decodes faulting instructions on
memory-access exits and calls back into the MMIO handler, which the NV2A and
future device models use; its known weakness on exotic SIMD store encodings
is a documented risk to revisit for the GPU aperture.

**Considered and deferred: full low-level emulation (LLE).** Running a real
MCPX ROM and Xbox kernel as guest code and emulating the silicon (PIC, timers,
SMBus, NV2A registers, APU DSPs) is the alternative "architecturally honest"
shape. It is deferred, not chosen, because it discards the completed HLE
kernel work, requires user-supplied proprietary ROM and flash to boot, and is
a much larger device-model project. The WHP execution core in this ADR is the
shared foundation for either device model, so an LLE pivot later reuses it.

## Milestone ladder

- WHP-M0: capability doctor, partition, vCPU, memory maps, exit dispatch; a
  32-bit protected-mode guest executes a trivial program and traps a
  thunk-gate call cleanly.
- WHP-M1: run the retail boot path on WHP to the same kernel wall the
  interpreter reaches, proving the HLE gate model under hardware
  virtualization.
- WHP-M2: retire the interpreter from the default runtime path (kept as the
  oracle/golden tier) once WHP reaches parity.
