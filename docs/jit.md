# Dynamic Recompiler Design

## Strategy

Use a tiered design.

The first tier directly rewrites guest x86 instructions for an x86-64 host. A later tier can use Cranelift for complex blocks.

Do not add Melior until a measured requirement justifies its LLVM and MLIR dependencies.

## Block formation

Decode with 32-bit `iced-x86` mode.

Stop a block at these boundaries:

- Direct or indirect control flow.
- Calls and returns.
- Interrupt and system instructions.
- Invalid instructions.
- Configured byte or instruction limits.
- A guest page boundary when fault metadata requires it.

Store the guest instruction address for each decoded instruction.

## Rewrite classes

Each instruction receives one class:

- Native candidate.
- Memory operation.
- Control flow.
- Helper call.
- Unsupported operation.

A native candidate has no guest memory access and no block-ending control flow.

Do not emit an instruction until its implicit register and memory effects are known.

## Host register contract

Reserve host registers only after measurements prove the need.

The initial direct emitter can spill guest state through a `CpuState` pointer.

A mature backend can cache hot guest registers. It must define a complete entry and exit contract.

Document these items before executable emission:

- Guest register locations.
- Host scratch registers.
- Host stack alignment.
- Flags ownership.
- Segment-base ownership.
- Helper-call preservation rules.

## Memory operands

Identity mode can retain many 32-bit effective addresses.

Arena mode adds a host base through explicit lowering or a segment prefix.

String operations need dedicated lowering. Their implicit source and destination segments need special handling.

Lock-prefixed operations need atomic host semantics.

## Control flow

Direct branches inside one translated unit can target local host labels.

External direct branches use a block-link stub or dispatcher.

Indirect branches use a lookup keyed by guest target and address-space epoch.

Calls must preserve guest return addresses. Host return addresses must remain separate.

## Code cache

Use write and execute phases, not simultaneous writable-executable pages.

A block key contains at least these values:

- Guest start address.
- Address-space epoch.
- Backend identifier.
- Relevant execution mode flags.

A block also records physical-page generation dependencies.

## Fault metadata

Each faultable host instruction needs a compact record.

The record contains these values:

- Host instruction range.
- Guest instruction address.
- Guest memory access type.
- Access width.
- Destination or source register.
- Resume host address.
- Slow-stub address.

Keep the exception handler allocation-free.

## Cranelift backend

The Cranelift backend can lower a normalized guest IR.

Do not lower raw `iced-x86` instructions directly into Cranelift throughout the codebase. Keep one normalized operation layer.

Use Cranelift first for blocks that contain complex flags, floating-point behavior, or difficult register pressure.

## Current implementation

The repository implements block decoding, instruction classification, planning, and cache metadata.

The platform crate owns executable memory. `WritableCodeBuffer` accepts code while writable and non-executable. `seal` consumes the writable owner, marks the pages execute-read, and flushes the host instruction cache. The sealed owner exposes no mutation.

The direct emitter translates the register-only subset from ADR 0006: `NOP`, register and immediate `MOV`, and register and immediate `ADD`, `SUB`, `AND`, `OR`, and `XOR`. Guest registers spill through the `CpuState` pointer, guest arithmetic flags materialize after every flag-writing instruction, and every block returns through one dispatcher exit. An interpreter oracle in `exbawks-cpu` verifies every operation and its flags.

Memory operands, control flow, and cross-block linking remain untranslated. Blocks that reach them exit with the unsupported-instruction code and the untranslated guest address.
