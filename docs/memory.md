# Guest Memory Design

## Goals

Normal guest RAM access must remain fast.

Guest aliases must stay coherent without copies. Invalid and MMIO pages must remain distinguishable.

HLE code must use checked guest addresses.

## Address types

Use these domains:

- `GuestVa` for guest virtual addresses.
- `GuestPa` for guest physical addresses.
- `GuestPage` for 4 KiB guest pages.
- Host pointers only inside host mapping code.

Do not cast between these domains without an explicit translation function.

## Sidecar page table

The 32-bit guest space contains 1,048,576 pages.

Each guest page descriptor uses one atomic 64-bit value. The descriptor table consumes 8 MiB.

A separate 16-bit generation value exists for each physical page. That table consumes 2 MiB for a 4 GiB physical range.

The complete metadata allocation consumes 10 MiB.

A guest page descriptor contains these fields:

- Page kind.
- Read, write, and execute permissions.
- Guest physical page number.
- MMIO handler identifier.
- Code generation value.
- Read, write, and execute watch flags.

The software backend consults this table for every access.

The Windows fast path consults it during mapping changes, HLE validation, faults, and invalidation.

Mapping functions preflight the complete range before descriptor updates. A rejected operation leaves the original map intact.

Readers use atomic descriptor loads. Readers can observe an update while a successful multi-page mutation applies.

## Windows section-view model

Create one pagefile-backed section for guest physical RAM.

Map the same section offsets into every required guest alias. Views from one section keep coherent bytes.

Reserve a high guest arena before large host allocations.

```text
host address = arena base + guest virtual address
```

Leave unmapped pages as placeholders or `PAGE_NOACCESS` regions.

The initial implementation will use `VirtualAlloc2` placeholders and `MapViewOfFile3` replacement views.

Placeholder split and coalesce operations keep one owner for every live range.

Windows 11 accepts placeholder splits and section-view replacements at 4 KiB page granularity. The platform tests verify this behavior.

## Identity map

An optional low identity map can make many guest operands directly usable.

Reserve only required guest intervals. Do not reserve the complete low 4 GiB range as one object.

Treat the identity map as an optimization. HLE code must never depend on it.

## Segment-base experiment

A later mode can place the high arena base in the host `FS` base.

A rewritten memory operand can then use an `FS` prefix. This method preserves the original ModRM and SIB address terms.

This mode requires strict thread pinning and host-state restoration.

Do not call Rust or Windows code with the guest `FS` base active.

## Mapping operations

All mapping operations must use page-aligned guest ranges.

Use these logical operations:

```text
map anonymous guest RAM
map a virtual alias to existing physical pages
change page permissions
unmap a guest range
reserve an MMIO range
register a fault handler identifier
```

The current page table implements RAM, MMIO, reserved, protection, watch, and unmap operations.

A future address-space epoch will identify structural mapping changes.

Each write to translated code must increment the generation for its physical page.

## Code invalidation

Track code dependencies by physical page.

A virtual alias can modify code that executes through another virtual address. Virtual-page tracking alone is incorrect.

Protect every alias when write-fault invalidation becomes active.

A compiled block records the generation of each physical code page. A mismatch invalidates the block.

Every checked guest write bumps the generation of each RAM page it covers,
in both address-space backends. Interpreter stores and future JIT helper
stores therefore invalidate cached code through one shared path; pages that
never backed executable mappings pay one atomic bump that no cached block
observes. Host write-fault detection for JIT direct stores arrives with the
arena work (ADR 0004).

## MMIO

Static MMIO addresses can lower directly to helper calls.

Dynamic MMIO addresses can fault through inaccessible arena pages.

A vectored exception handler must recognize only registered JIT fault sites. It must redirect to a generated slow stub.

Do not perform complex HLE work inside the exception handler.

## Current implementation

`SoftwareAddressSpace` provides these operations:

- Contiguous physical allocation.
- Anonymous virtual mapping.
- Coherent virtual aliases.
- Page permission changes.
- Checked reads and writes.
- Execute-permission fetches.
- Explicit physical generation updates.
- Allocation rollback after a failed map.

`WindowsArenaPlan` validates arena alignment, physical ranges, view alignment, and guest overlap.

`WindowsAddressSpace` provides these operations on Windows:

- One pagefile-backed physical RAM section.
- A reserved 4 GiB-aligned guest arena.
- Anonymous mappings and coherent aliases through replaced placeholder views.
- Sidecar page-table records for every view.
- Unmapping that restores and coalesces free placeholders.
- Checked reads, writes, and execute-permission fetches through mapped views.
- Guest permission changes through the sidecar page table.

Both backends validate every access through one shared sidecar walk. Generated equivalence tests require identical typed failures and identical readable bytes from both backends.

Checked reads share one backend lock. Checked writes and permission changes hold it exclusively, so raw view copies never race and permission changes never interleave with an in-flight access.

Windows caps view protections at the map-time protection class. Views that need host execute permission must map with an execute class.

Arena RAM views keep host read-write protection. The sidecar page table is the permission authority for checked access on both backends. Host protection tightening arrives with write-fault code invalidation. The platform view wrapper already supports host protection changes.
