# ADR 0003: Direct Rewriting Before General IR

- Status: accepted
- Date: 2026-08-15

## Context

The guest and host both use x86 instruction families.

Many guest operations can retain much of their original structure.

A general compiler backend adds translation and integration cost.

## Decision

Implement a direct `iced-x86` rewrite backend first.

Keep code generation behind a backend trait.

Add Cranelift for blocks that benefit from a normalized IR and register allocation.

Do not add Melior during the first milestones.

## Consequences

The first backend can reach low overhead for common operations.

Complex flags, floating-point behavior, and helper-heavy blocks need careful lowering.

The normalized operation layer must not depend on one backend.
