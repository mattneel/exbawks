# ADR 0005: Track Code by Physical Page

- Status: accepted
- Date: 2026-08-15

## Context

One guest physical page can appear at multiple guest virtual addresses.

Guest code can execute through one alias and write through another alias.

## Decision

Record translated block dependencies by guest physical page.

Store a generation number for each dependency.

Invalidate all dependent blocks when a physical code page changes.

Protect every writable alias when write-fault invalidation becomes active.

## Consequences

Virtual aliases cannot bypass self-modifying code detection.

The memory subsystem must expose physical-page identity to the code cache.
