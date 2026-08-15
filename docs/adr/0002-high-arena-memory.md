# ADR 0002: High Guest Arena and Coherent Views

- Status: accepted
- Date: 2026-08-15

## Context

Guest virtual aliases must reference the same physical bytes.

A software translation lookup on every guest access adds high overhead.

## Decision

Create one pagefile-backed section for guest physical RAM.

Reserve a high 4 GiB guest arena.

Map section offsets into arena positions that match guest virtual addresses.

Keep a sidecar page table for validation, faults, MMIO, and invalidation.

Allow an optional low identity view as an optimization.

## Consequences

Normal mapped RAM can use hardware address translation.

Virtual aliases remain coherent through shared section views.

Sparse mapping logic becomes a security-sensitive subsystem.

HLE code must use the high arena or checked memory interface.
