# ADR 0001: Windows 11 Runtime Baseline

- Status: accepted
- Date: 2026-08-15

## Context

The memory design depends on modern Windows virtual-memory APIs.

Early portability work can slow the first executable milestone.

## Decision

Target Windows 11 on x86-64 for the first runtime.

Keep parser, decoder, planner, and software-memory crates portable.

Use the MSVC Rust target for release builds.

## Consequences

The runtime can use `VirtualAlloc2`, placeholder views, vectored exceptions, and modern graphics APIs.

Non-Windows hosts can run pure logic tests. They cannot run the emulator backend.

A later ADR must define each new host platform.
