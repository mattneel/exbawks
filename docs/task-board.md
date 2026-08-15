# Implementation Task Board

## Use

Create one issue for each task identifier.

Keep one task per pull request unless the task states otherwise.

Run `cargo xtask check` before each merge.

## Dependency chain

```text
MEM-001 -> MEM-002 -> MEM-003 -> MEM-004 -> MEM-005
                                      |
                                      +-> JIT-001 -> JIT-002 -> JIT-003
XBE-001 -> HLE-001 -> HLE-002 --------+
DBG-001 ------------------------------+
```

## Phase 1: Windows address space

### MEM-001: Query host memory geometry

**Goal:** Expose the host page size and allocation granularity.

**Files:** `crates/exbawks-platform/src`.

**Work:**

1. Add a `SystemMemoryInfo` value type.
2. Call `GetSystemInfo` on Windows.
3. Return an unsupported error on other hosts.
4. Add Windows and portable tests.

**Acceptance:**

- Both values are nonzero powers of two.
- The allocation granularity is not smaller than the page size.
- The `doctor` command prints both values.

### MEM-002: Add placeholder split and coalesce operations

**Goal:** Manage sparse regions inside one reserved arena.

**Files:** `crates/exbawks-platform/src/virtual_memory`.

**Work:**

1. Validate every host range before a Windows call.
2. Split a placeholder with `MEM_PRESERVE_PLACEHOLDER`.
3. Coalesce adjacent placeholders with `MEM_COALESCE_PLACEHOLDERS`.
4. Keep one owner for every live range.
5. Add failure-path tests.

**Acceptance:**

- A test splits one range into three owned ranges.
- A test coalesces the three ranges.
- A failed split preserves valid ownership.
- No owner implements `Clone`.

### MEM-003: Reserve the high guest arena

**Goal:** Reserve one 4 GiB-aligned, 4 GiB host range.

**Files:** `crates/exbawks-memory`, `crates/exbawks-platform`.

**Work:**

1. Search aligned candidate addresses with `VirtualAlloc2` constraints.
2. Reserve the complete arena as a placeholder.
3. Keep unmapped guest pages inaccessible.
4. Report the selected base through tracing.

**Acceptance:**

- The arena base has 4 GiB alignment.
- `base + guest_va` produces every arena address.
- Arena destruction releases the complete reservation.

### MEM-004: Map coherent RAM aliases

**Goal:** Map one physical section offset into multiple guest ranges.

**Files:** `crates/exbawks-memory`.

**Work:**

1. Create one pagefile-backed physical RAM section.
2. Replace selected placeholders with section views.
3. Record each view in the sidecar page table.
4. Restore placeholders during unmap operations.

**Acceptance:**

- A write through one alias appears through another alias.
- Unmap one alias without changing the other alias.
- Reject overlapping guest view plans.
- Reject section ranges outside physical RAM.

### MEM-005: Implement the Windows `GuestMemory` backend

**Goal:** Match software backend behavior through mapped Windows views.

**Files:** `crates/exbawks-memory`.

**Work:**

1. Implement checked reads, writes, and fetches.
2. Use the sidecar table for permission validation.
3. Add protection changes for mapped views.
4. Keep helper code independent from identity mappings.

**Acceptance:**

- Generated mapping tests pass against both backends.
- Both backends return the same typed failures.
- Execute fetches require execute permission.

## Phase 2: First executable backend

### JIT-001: Define executable memory ownership

**Goal:** Enforce separate write and execute phases.

**Files:** `crates/exbawks-platform`, `crates/exbawks-jit`.

**Work:**

1. Add writable code-buffer ownership.
2. Seal a buffer as execute-read memory.
3. Flush the instruction cache after sealing.
4. Prevent mutation after sealing.

**Acceptance:**

- No executable page remains writable.
- A sealed buffer executes a fixed return stub.
- Drop releases every allocation.

### JIT-002: Implement register-only direct emission

**Goal:** Execute the first safe translated instruction subset.

**Files:** `crates/exbawks-jit`, `crates/exbawks-cpu`.

**Supported operations:**

- `NOP`.
- Register `MOV`.
- Register and immediate `ADD`.
- Register and immediate `SUB`.
- Register and immediate `AND`.
- Register and immediate `OR`.
- Register and immediate `XOR`.

**Acceptance:**

- Each operation matches an interpreter oracle.
- Each test verifies affected flags.
- Each block returns through one dispatcher exit.

### JIT-003: Add source and fault metadata

**Goal:** Map generated host locations to guest instructions.

**Files:** `crates/exbawks-jit`, `crates/exbawks-debug`.

**Work:**

1. Add source ranges to `CompiledBlock`.
2. Add fault-site records for faultable instructions.
3. Use immutable sorted metadata after block sealing.
4. Add binary-search lookup tests.

**Acceptance:**

- Every emitted guest instruction has one source range.
- Every faultable host instruction has one fault record.
- Unknown host locations return no match.

## Phase 3: XBE and kernel startup

### XBE-001: Expand checked XBE metadata

**Goal:** Parse certificate, TLS, library, and debug metadata.

**Files:** `crates/exbawks-xbe`.

**Acceptance:**

- Every address uses a checked file or guest range.
- Parser limits cap counts and string lengths.
- Fuzz inputs cannot request unbounded allocation.

### HLE-001: Patch kernel thunk gates

**Goal:** Replace imported ordinals with controlled dispatch gates.

**Files:** `crates/exbawks-core`, `crates/exbawks-kernel`, `crates/exbawks-jit`.

**Acceptance:**

- A synthetic thunk calls one registered export.
- An unknown ordinal returns a named stop reason.
- A malformed thunk remains a typed load error.

### HLE-002: Add the startup export set

**Goal:** Support one synthetic guest thread through a kernel call.

**Initial groups:**

- Virtual memory.
- Threads.
- Events.
- Timers.
- Files.
- Debug output.

**Acceptance:**

- A synthetic XBE reaches its entry point.
- It calls one HLE export.
- It returns to translated guest code.
- It exits with a controlled stop reason.

## Cross-cutting tasks

### DBG-001: Add a JSON Lines trace writer

**Goal:** Persist deterministic structured events.

**Files:** `crates/exbawks-debug`, `apps/exbawks-cli`.

**Acceptance:**

- Each line contains one valid JSON object.
- Each event includes a sequence number.
- Private host paths are optional fields.

### QA-001: Add fuzz workspace scaffolding

**Goal:** Add bounded parser fuzz targets.

**Targets:**

- XBE images.
- Kernel thunk tables.
- Future push-buffer packets.

**Acceptance:**

- `cargo fuzz run xbe -- -max_total_time=30` starts successfully.
- Corpus files contain no proprietary data.

### REL-001: Create the first tagged development release

**Goal:** Publish source and synthetic validation results.

**Acceptance:**

- Complete the release checklist.
- Commit `Cargo.lock`.
- Attach no Xbox software or console data.
