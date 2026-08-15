# Security Model

## Scope

Exbawks processes untrusted XBE files and untrusted guest memory values.

The emulator is not a security boundary for hostile native host code.

The first runtime targets a single local user on Windows 11.

## Trust boundaries

Treat these inputs as untrusted:

- XBE files.
- Guest pointers and lengths.
- Guest instruction bytes.
- Kernel thunk values.
- Graphics packet data.
- Save states and trace imports.

Treat generated machine code as security-sensitive output.

## File parsing

Check every addition and multiplication before slice access.

Cap every file-controlled count and string length.

Do not allocate from an unchecked file value.

Do not use `transmute` for file parsing.

## Guest memory

Use typed guest address values at subsystem boundaries.

Validate permissions before every HLE memory access.

Keep unmapped and MMIO arena pages inaccessible.

Do not return a host pointer to guest code.

## Dynamic code

Keep code pages writable or executable, but never both.

Validate each emitted branch target.

Store source maps and fault maps outside executable memory.

Reject a fault that does not match registered JIT metadata.

## Windows exceptions

Keep the vectored exception handler allocation-free.

Do not call complex Rust code from the handler.

Redirect only expected JIT faults to generated slow stubs.

Leave unrelated access violations as host failures.

## Unsafe Rust

Keep unsafe code inside `exbawks-platform` and future emitter modules.

Add one `SAFETY:` comment before every unsafe operation.

State the range, lifetime, alignment, ownership, and thread invariant.

Do not hide unsafe operations inside broad utility modules.

## HLE boundaries

Copy small guest structures into host-owned values.

Use scoped access guards for larger guest buffers.

Do not retain a guest slice after an HLE call returns.

Validate handles through typed object tables.

## Secrets and proprietary data

Do not log keys or full private guest paths by default.

Do not commit Xbox software, firmware, keys, or extracted game data.

Keep private fixtures under `fixtures/private`.

## Reporting

Use the process in `SECURITY.md` for a suspected host security defect.

Do not publish a working exploit before coordinated review.
