# ADR 0010: Kernel-Visible Guest Address Map

- Status: accepted
- Date: 2026-08-15

## Context

Retail titles observe kernel-provided addresses: data-export thunk slots,
`MmAllocateContiguousMemory` results, thread stacks, and the KPCR reached
through `fs:`. Games compare pointers against `0x8000_0000`, and on hardware
every kernel-provided object lives in kernel space. Exbawks currently patches
every thunk slot with an unmapped gate address, maps one fixed 64 KiB user
stack at `0x03FF_0000`, and has no KPCR, so XAPI startup cannot run. The 19
DATA exports Dino Crisis 3 imports are dereferenced as data pointers and must
resolve to mapped guest memory.

## Decision

The kernel HLE presents this guest virtual layout:

- User range: `0x0001_0000` through `0x7FFF_FFFF`. The XBE image, its heap
  reservations, and future `NtAllocateVirtualMemory` results stay here.
- Cached physical window: virtual address `0x8000_0000 | physical`, covering
  configured RAM (64 MiB retail: `0x8000_0000` through `0x83FF_FFFF`). Every
  kernel-provided pointer is a window address, so guest comparisons against
  `0x8000_0000` behave as on hardware.
- Write-combined physical window: `0xF000_0000 | physical`, reserved now,
  mapped when `MmAllocateContiguousMemoryEx` honors write-combined protection.
- Kernel gate region: `0xFF80_0000 + ordinal * 4` stays reserved and unmapped
  for function-export dispatch. The region aliases the hardware flash
  aperture; a title probing flash would observe gate addresses, which stays a
  documented non-goal.

Kernel-owned objects allocate physical pages from the top of RAM downward and
are addressed through the cached window:

- One kernel-variables block holds every DATA export: a ticking `KeTickCount`
  cell, `KeTimeIncrement`, `LaunchDataPage` (zero on cold boot),
  `XeImageFileName` as an ANSI string resolving to
  `\Device\CdRom0\default.xbe`, zeroed synthetic key blocks, fixed
  `XboxKrnlVersion`/`XboxHardwareInfo` values, and opaque object-type records.
  The loader patches DATA-export thunk slots with these window addresses;
  function slots keep gate addresses. The kind comes from the generated
  ordinal table.
- One KPCR/TIB page per guest thread. `fs` base points at it; `fs:[0]` holds
  the SEH list head, `fs:[4]`/`fs:[8]` the stack bounds, `fs:[0x1C]` the
  self pointer, and `fs:[0x28]` the current-thread pointer.
- Thread stacks, sized from the XBE header for thread zero and from the
  creation argument for later threads, each with one unmapped guard page
  below the limit. The fixed `0x03FF_0000` user stack is retired when
  `CORE-003` lands.

Until the real allocator tasks land (`MEM-006`, `MEM-007`), the loader
reserves these blocks with the existing bump allocator; the layout contract
is what this ADR fixes, not the allocation mechanism.

## Consequences

Data exports become readable guest memory and `KeTickCount` can tick without
any HLE boundary, unblocking XAPI timing paths.

`patch_kernel_thunks` changes behavior for DATA ordinals, amending the
HLE-001 design that every slot receives a gate address; the gate mechanism
itself is unchanged for functions.

The physical windows make `MmGetPhysicalAddress` a mask operation and give
`MmAllocateContiguousMemory` hardware-shaped return values before the real
allocator exists.

Window mappings are aliases of RAM pages, so ADR 0005 physical-page code
invalidation applies unchanged; executable user mappings and window views of
the same physical page share generations.

Anything later mapped in `0x8400_0000` through `0xEFFF_FFFF` (page-table
self-map, system PTE ranges) needs an ADR amendment; nothing in the title
screen plan requires it.
