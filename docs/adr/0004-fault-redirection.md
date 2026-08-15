# ADR 0004: Redirect Expected Faults to Generated Stubs

- Status: proposed
- Date: 2026-08-15

## Context

Dynamic MMIO and invalid guest accesses can use inaccessible host pages.

Complex emulator work inside a vectored exception handler is unsafe.

## Decision

Store fault metadata for each generated faultable instruction.

The exception handler will match the host instruction pointer.

It will redirect execution to a generated slow stub.

The slow stub will restore host state before a Rust helper call.

## Consequences

Normal RAM access needs no explicit branch.

Fault metadata and source maps become required codegen outputs.

Unknown host faults remain fatal emulator defects.
