# Architecture

## Purpose

Exbawks separates guest architecture rules from host operating-system details.

The first runtime target is Windows 11 on x86-64. Portable components remain host-neutral when practical.

## Subsystems

### XBE loader

The loader validates file ranges before allocation.

It decodes the entry point and kernel thunk address. It maps headers and sections into guest memory.

The loader does not verify Microsoft signatures. It does not bypass media or license controls.

### Guest memory

Guest addresses use explicit newtypes.

The sidecar page table contains one entry for each 4 KiB guest page. Each entry stores mapping, permission, generation, and watch data.

The software backend provides deterministic tests. The Windows backend will use coherent section views for the fast path.

### CPU frontend

The CPU frontend owns guest architectural state.

`iced-x86` decodes 32-bit x86 instructions. The block decoder stops at control flow or a configured limit.

The frontend produces a normalized block for a codegen backend.

### Code generation

The direct backend classifies each instruction before emission.

Register-only operations can stay close to their guest form. Memory, control flow, and privileged operations require explicit lowering.

The backend interface permits a later Cranelift implementation. Melior remains a research option.

### Kernel HLE

The kernel registry maps import ordinals to typed host callbacks.

Each callback receives guest CPU state and a checked guest memory interface.

Callbacks must not retain guest slices after the call returns.

### Graphics HLE

The graphics frontend converts guest API calls and push-buffer operations into host-neutral commands.

A host backend consumes those commands. The initial backend is a null implementation for tests.

### Debug support

The debug crate owns breakpoints and structured trace events.

The code cache will provide guest-to-host source maps. Fault metadata will provide the reverse map.

## Dependency direction

```text
exbawks-types
    ^
    +-- exbawks-platform
    +-- exbawks-xbe
    +-- exbawks-cpu
    +-- exbawks-debug
    +-- exbawks-gpu

exbawks-memory --> exbawks-platform
exbawks-jit    --> exbawks-cpu + exbawks-memory + exbawks-platform + exbawks-debug
exbawks-kernel --> exbawks-cpu + exbawks-memory
exbawks-core   --> all subsystem crates
exbawks-cli    --> exbawks-core and inspection crates
```

Lower crates must not depend on `exbawks-core`.

## Runtime flow

1. Parse and validate the XBE file.
2. Allocate guest physical pages.
3. Map XBE headers and sections.
4. Decode the kernel thunk table.
5. Set the initial guest CPU state.
6. Decode the first basic block.
7. Find or compile a cached host block.
8. Execute the block.
9. Dispatch HLE, MMIO, or fault exits.
10. Continue until a stop reason occurs.

## Thread model

The first implementation uses one guest CPU thread.

The execution thread owns guest register state. It also owns host segment-base state when that address mode becomes active.

Helper calls can use worker threads only for host work. They must not mutate guest CPU state without synchronization.

## State ownership

The emulator object owns all subsystem roots.

Guest memory can use shared ownership because HLE components need access. Guest CPU state remains execution-thread state.

The code cache uses physical-page dependencies. This rule preserves correctness across virtual aliases.

## Failure model

Malformed files return typed errors.

Unsupported guest instructions produce a controlled JIT exit. Missing HLE exports produce a named dispatch error.

Host access violations outside registered JIT fault sites remain host failures.
