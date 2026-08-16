# ADR 0008: Interpreter Execution Tier

- Status: accepted
- Date: 2026-08-15

## Context

The direct backend translates only the ADR 0006 register-only subset. A retail
XBE stops at its first entry instruction because memory operands, control flow,
string operations, segment overrides, and floating-point instructions have no
execution path, and the run loop halts on any untranslated instruction.

The existing interpreter oracle shares `classify_register_op` with the emitter,
so it can never verify coverage the emitter does not already have. Every new
JIT instruction family needs an independent reference implementation.

Extending JIT coverage instruction family by instruction family while the run
loop halts on every gap couples boot progress to emission progress, which the
Dino Crisis 3 boot plan measures as the single largest schedule risk.

## Decision

`exbawks-cpu` gains a complete user-mode x86 interpreter as execution tier 0.

The interpreter steps one instruction at a time over `&mut CpuState` and a
`&dyn GuestMemory`, covering the full Pentium III user-mode profile in staged
tasks: memory operands and ALU forms, control flow and stack traffic, string
operations and segment overrides, CPUID and RDTSC, x87, MMX, and SSE1.

The run loop treats the interpreter as the fallback tier: when a translated
block exits with an untranslated instruction, the loop first probes the kernel
gate, then interprets bounded steps until execution re-enters translatable
territory. Typed errors replace halts; malformed guest state never panics.

The interpreter is the permanent differential oracle. Every JIT emission stage
lands with tests that run the same instruction stream through both tiers and
compare complete architectural state. Divergence is a test failure, never a
silent fallback.

Interpreted stores participate in code invalidation: guest writes bump the
physical-page generation through the shared address-space write path, the same
path future JIT helper-call stores use, so ADR 0005 revalidation observes
self-modifying code from either tier.

CPUID returns a fixed Pentium III profile (family 6, MMX, SSE, no SSE2).
RDTSC reads the deterministic emulator counter, never the host time-stamp
counter; the virtual-clock ADR formalizes the counter.

## Consequences

Boot depth decouples from JIT coverage: the retail image can execute its full
XAPI startup under the interpreter while emission catches up family by family.

Interpretation is slow. Real-time media decode and title-screen frame rate
still require the JIT stages; the boot plan tracks them as explicit milestone
entry criteria, with an FMV throughput measurement as the trigger.

Two execution paths can drift. The shared differential corpus runs in both
directions on every stage, and the run-loop fallback surfaces divergence as
behavioral differences in deterministic traces rather than silent corruption.

The interpreter step signature takes guest memory, so `exbawks-cpu` remains a
pure logic crate and the portable Linux CI job exercises the full interpreter
against the software address space.
