# Roadmap

## Phase 0: Scaffold

Status: complete.

Acceptance criteria:

- The workspace contains subsystem boundaries.
- Synthetic tests cover the XBE parser and software memory.
- The CLI can inspect an XBE and plan an entry block.
- CI runs format, lint, test, and documentation checks.

## Phase 1: Windows address space

Status: next.

Acceptance criteria:

- Reserve a 4 GiB high guest arena with required alignment.
- Split and coalesce placeholders safely.
- Map one physical RAM section through multiple coherent views.
- Change permissions at guest-page granularity.
- Keep unmapped and MMIO pages inaccessible.
- Pass generated alias and permission tests on Windows 11.

## Phase 2: Executable direct backend

Acceptance criteria:

- Allocate code with separate write and execute phases.
- Emit register-only integer operations.
- Preserve the guest CPU state contract.
- Return through one dispatcher exit stub.
- Provide guest-to-host source maps.
- Compare results against a small interpreter oracle.

## Phase 3: Guest memory operations

Acceptance criteria:

- Lower common loads and stores in identity or arena mode.
- Handle 32-bit address wrap correctly.
- Lower string and locked operations through dedicated paths.
- Add physical-page write invalidation.
- Pass alias self-modification tests.

## Phase 4: Fault and MMIO path

Acceptance criteria:

- Register a vectored exception handler.
- Match only generated fault sites.
- Redirect to allocation-free slow stubs.
- Resume loads and stores with correct widths and extensions.
- Preserve host and guest thread state.

## Phase 5: Kernel startup HLE

Acceptance criteria:

- Parse and patch kernel thunks.
- Implement memory, thread, event, timer, file, and debug exports.
- Boot a synthetic XBE into its entry function.
- Complete one HLE call and return to guest code.

## Phase 6: Graphics startup

Acceptance criteria:

- Intercept device creation.
- Track guest graphics state.
- Create a host window and backend device.
- Clear and present a synthetic frame.
- Record unknown calls and push-buffer methods.

## Phase 7: Compatibility expansion

Acceptance criteria:

- Add title-specific compatibility profiles only with documented evidence.
- Add deterministic input, audio, storage, and timing services.
- Add a Cranelift backend for selected difficult blocks.
- Publish reproducible performance and compatibility reports.
