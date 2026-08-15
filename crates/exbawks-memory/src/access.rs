use std::cmp;

use exbawks_types::{AccessKind, GUEST_PAGE_SIZE, GuestPage, GuestVa};

use crate::{MemoryError, PageDescriptor, PageKind, PageTable};

/// A borrowed buffer for one guest access.
pub(crate) enum AccessBuffer<'a> {
    Read(&'a mut [u8]),
    Write(&'a [u8]),
}

impl AccessBuffer<'_> {
    pub(crate) const fn len(&self) -> usize {
        match self {
            Self::Read(value) => value.len(),
            Self::Write(value) => value.len(),
        }
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Loads one descriptor and validates kind and permissions for an access.
pub(crate) fn checked_descriptor(
    table: &PageTable,
    address: GuestVa,
    access: AccessKind,
) -> Result<PageDescriptor, MemoryError> {
    let descriptor = table.get(address.page());
    if !descriptor.is_valid() || descriptor.kind() == PageKind::Unmapped {
        return Err(MemoryError::Unmapped { address, access });
    }

    if !descriptor.permissions().contains(access.required_permission()) {
        return Err(MemoryError::AccessDenied { address, access });
    }

    match descriptor.kind() {
        PageKind::Ram => Ok(descriptor),
        PageKind::Mmio => Err(MemoryError::Mmio { address, handler_id: descriptor.handler_id() }),
        PageKind::Reserved | PageKind::Unmapped => Err(MemoryError::Unmapped { address, access }),
    }
}

/// Walks page-sized chunks of one validated access range.
///
/// The visitor receives the chunk start address, its checked descriptor, the
/// buffer offset, and the chunk length.
pub(crate) fn walk_pages(
    table: &PageTable,
    access: AccessKind,
    address: GuestVa,
    len: usize,
    mut visit: impl FnMut(GuestVa, PageDescriptor, usize, usize) -> Result<(), MemoryError>,
) -> Result<(), MemoryError> {
    let mut guest = address.0;
    let mut buffer_offset = 0_usize;

    while buffer_offset < len {
        let current = GuestVa(guest);
        let descriptor = checked_descriptor(table, current, access)?;
        let page_remaining = usize::try_from(GUEST_PAGE_SIZE - current.page_offset())
            .map_err(|_| MemoryError::HostSizeOverflow)?;
        let chunk = cmp::min(page_remaining, len - buffer_offset);
        visit(current, descriptor, buffer_offset, chunk)?;

        buffer_offset += chunk;
        if buffer_offset < len {
            guest = guest
                .checked_add(u32::try_from(chunk).map_err(|_| MemoryError::HostSizeOverflow)?)
                .ok_or(MemoryError::HostSizeOverflow)?;
        }
    }

    Ok(())
}

/// Returns the host byte offset of one physical page position.
pub(crate) fn physical_offset(page: GuestPage, page_offset: u32) -> Result<usize, MemoryError> {
    let offset = u64::from(page.0)
        .checked_mul(u64::from(GUEST_PAGE_SIZE))
        .and_then(|base| base.checked_add(u64::from(page_offset)))
        .ok_or(MemoryError::HostSizeOverflow)?;
    usize::try_from(offset).map_err(|_| MemoryError::HostSizeOverflow)
}

/// Rounds a byte count up to a page multiple.
pub(crate) fn align_up(value: u64, alignment: u64) -> Result<u64, MemoryError> {
    let mask = alignment - 1;
    value.checked_add(mask).map(|sum| sum & !mask).ok_or(MemoryError::HostSizeOverflow)
}
