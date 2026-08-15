# XBE Loader

## Responsibilities

The XBE loader validates file structure and produces guest mappings.

It does not execute code. It does not load external assets automatically.

## Header fields

The parser reads these initial fields:

- Magic.
- Image base.
- Header and image sizes.
- Certificate address.
- Section count and section-table address.
- Initialization flags.
- Encoded entry point.
- TLS address.
- Stack and heap values.
- Encoded kernel thunk address.

The parser keeps unknown fields available for later work when practical.

## Address decoding

Retail and debug XBE files use different XOR constants for entry and thunk addresses.

The parser tests decoded entry candidates against the image range. It then uses the same flavor for the thunk address.

Reject a file when no candidate lies inside the image.

## Section validation

For each section, verify these conditions:

- The section header lies inside the file.
- The raw byte range lies inside the file.
- The virtual range does not overflow the 32-bit guest space.
- The name address maps into the header range.
- The section count stays below the configured limit.

Map `virtual_size` bytes and copy `raw_size` bytes.

Zero the remaining virtual bytes.

## Permissions

Map readable sections with read permission.

Add write permission for writable sections. Add execute permission for executable sections.

Map headers as read-only after relocation and thunk preparation.

## Kernel thunks

Read the thunk table after section mapping.

Validate each table entry and the zero terminator.

Resolve ordinals through the kernel HLE registry.

The current scaffold parses the thunk address. It does not patch the table yet.

## Certificates and signatures

Certificate metadata is useful for title identity and compatibility rules.

Add certificate parsing as a separate checked structure.

Do not implement signature bypass features. Exbawks loads only files that the user supplies lawfully.
