# ADR 0007: Contiguous XBE Section Mapping and Shared-Page Permissions

- Status: accepted
- Date: 2026-08-15

## Context

Retail XBE sections are not page-aligned. Only the first section starts on a
page boundary. Every later section starts immediately after the previous
section's last byte, so adjacent sections share the 4 KiB page at their
boundary. The `HEAD_PAGE_READ_ONLY` and `TAIL_PAGE_READ_ONLY` section flags
exist to control the permissions of those shared boundary pages.

The initial loader assumed page-aligned, non-overlapping sections and mapped
one fresh anonymous region per section. That model rejects every retail image
at its second section and cannot represent a page shared by two sections.

## Decision

Map the union of the header range and every section as guest RAM, then set
each page's permissions from the merge of the sections that touch it.

1. Treat the headers as a read-only pseudo-section spanning
   `[base_address, base_address + size_of_headers)`.
2. Compute the covered guest pages as the union of the header range and every
   non-empty section range. Map each maximal run of contiguous covered pages
   as one anonymous writable RAM region.
3. Copy the header bytes and each section's raw bytes to their exact,
   possibly unaligned, guest virtual addresses. Bytes past a section's raw
   size stay zero.
4. Compute the final permission of each covered page as the union across
   every touching contributor:
   - Read is always granted.
   - Execute is granted when a touching section is `EXECUTABLE`.
   - Write is granted when a touching section is `WRITABLE` and does not mark
     that page as a read-only boundary. A section suppresses its own write
     contribution on its head page when it sets `HEAD_PAGE_READ_ONLY`, and on
     its tail page when it sets `TAIL_PAGE_READ_ONLY`.
   Apply the merged permission to each page.
5. Reject an image whose section byte ranges overlap by more than a shared
   page boundary, and reject a section whose raw size exceeds its virtual
   size or whose range leaves the declared image.

## Consequences

The loader accepts real retail images. A page shared by two sections holds
both sections' bytes and carries the union of their permissions.

This is a high-level-emulation permission policy, not a transcription of Xbox
hardware behavior. It grants write to a shared page when any writable section
legitimately writes it, which is the permissive-but-safe choice for guest
state that never becomes host-executable. A title that depends on a stricter
boundary protection would need a revised policy and a new ADR.

Guest arena RAM stays host read-write regardless of these guest permissions
(ADR 0002); the sidecar page table remains the permission authority for
checked guest access.
