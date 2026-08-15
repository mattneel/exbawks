# Code Generation Contract

## Status

This contract is a draft for the first executable backend.

Record material changes in a new ADR before executable code lands.

## Entry contract

The dispatcher calls generated code through one architecture-specific entry stub.

The entry stub receives one pointer to `CpuState`.

ADR 0006 fixes the first ABI: `extern "C" fn(*mut CpuState) -> u64` on x86-64 Windows. The pointer arrives in `RCX` and stays there for the block lifetime. First-subset blocks are leaf functions with `RAX` and `RDX` as scratch registers.

## Guest values

Guest general-purpose values use their low 32 bits.

Every 32-bit write clears the upper host bits before later pointer use.

Guest effective addresses use 32-bit wrapping arithmetic.

Host pointers never enter guest register state.

## Flags

The backend must preserve guest-visible arithmetic flags.

A native instruction can retain host flags until the next flag consumer.

A helper call must materialize required guest flags before the call.

The first executable subset must test these flags:

- Carry.
- Parity.
- Auxiliary carry.
- Zero.
- Sign.
- Overflow.

## Stack

Generated code uses a host stack that follows the Windows x64 ABI.

The entry stub reserves required shadow space.

The stack remains 16-byte aligned at every helper call.

Guest `ESP` remains a value inside `CpuState` until stack lowering lands.

## Host register classes

Use these logical classes:

- The `CpuState` pointer.
- The guest arena base, when required.
- Temporary values.
- Helper arguments.
- Dispatcher exit values.

Do not assign fixed registers in multiple modules.

Keep the assignment in one backend contract type.

## Helper calls

Materialize live guest state before a Rust or Windows helper call.

Restore host segment state before the helper call.

Do not keep borrowed guest slices across the helper call.

Return through a generated continuation or the dispatcher.

## Block exits

Every generated block returns one structured exit value.

Initial exit kinds are:

- Direct successor.
- Conditional successor.
- Indirect successor.
- Kernel HLE call.
- Memory slow path.
- Unsupported instruction.
- Budget exhaustion.

The exit value returns in `RAX`. The low 32 bits select the exit kind in the order above, starting at zero. The high 32 bits stay zero. The epilogue writes the successor `EIP` into `CpuState` before the return.

The dispatcher owns cross-block linking until safe patching lands.

## Executable memory

Write code only while a buffer is writable and non-executable.

Seal the buffer as execute-read before dispatch.

Flush the host instruction cache after each seal or patch.

Do not expose mutable slices after sealing.

## Metadata

Store metadata outside executable pages.

Each compiled block must contain:

- A guest start address.
- A guest end address.
- Physical-page dependencies.
- Source ranges.
- Fault sites.
- Exit records.
- The selected address mode.

The first executable subset carries the guest start, the exit guest address,
the static exit kind, source ranges, and fault sites. Physical-page
dependencies live on the code-cache entry, not the emitted block. The
register-only subset needs no memory address mode and emits no fault sites,
so the address-mode field is deferred until memory operands land.

## Address modes

The first implementation can use identity mappings for supported ranges.

The core contract uses the high arena as the stable memory model.

A later FS-base mode requires thread pinning and strict host-state restoration.
