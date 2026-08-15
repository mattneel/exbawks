use std::cmp;

use exbawks_types::{
    AccessKind, GUEST_PAGE_SIZE, GuestPa, GuestPage, GuestRange, GuestVa, MemoryPermissions,
};
use parking_lot::{Mutex, RwLock};

use crate::{MemoryError, PageKind, PageTable};

/// Checked guest memory access used by CPU and HLE components.
pub trait GuestMemory: Send + Sync {
    /// Reads guest data with read permission.
    fn read(&self, address: GuestVa, output: &mut [u8]) -> Result<(), MemoryError>;

    /// Reads guest instruction bytes with execute permission.
    fn fetch(&self, address: GuestVa, output: &mut [u8]) -> Result<(), MemoryError>;

    /// Writes guest data with write permission.
    fn write(&self, address: GuestVa, input: &[u8]) -> Result<(), MemoryError>;

    /// Returns the sidecar page table.
    fn page_table(&self) -> &PageTable;

    /// Reads one little-endian 32-bit value.
    fn read_u32(&self, address: GuestVa) -> Result<u32, MemoryError> {
        let mut bytes = [0_u8; 4];
        self.read(address, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    /// Writes one little-endian 32-bit value.
    fn write_u32(&self, address: GuestVa, value: u32) -> Result<(), MemoryError> {
        self.write(address, &value.to_le_bytes())
    }
}

#[derive(Debug)]
struct PhysicalAllocator {
    next_page: u32,
    page_count: u32,
}

impl PhysicalAllocator {
    fn allocate(&mut self, requested_pages: u32) -> Result<GuestPage, MemoryError> {
        let end = self
            .next_page
            .checked_add(requested_pages)
            .ok_or(MemoryError::OutOfPhysicalMemory { requested_pages })?;
        if end > self.page_count {
            return Err(MemoryError::OutOfPhysicalMemory { requested_pages });
        }

        let start = GuestPage(self.next_page);
        self.next_page = end;
        Ok(start)
    }

    fn rollback_last(&mut self, start: GuestPage, page_count: u32) {
        let expected_end =
            start.0.checked_add(page_count).expect("a completed allocation cannot overflow");
        assert_eq!(self.next_page, expected_end, "only the latest allocation can roll back");
        self.next_page = start.0;
    }
}

/// A deterministic software implementation of the guest address space.
#[derive(Debug)]
pub struct SoftwareAddressSpace {
    physical: RwLock<Box<[u8]>>,
    table: PageTable,
    allocator: Mutex<PhysicalAllocator>,
}

impl SoftwareAddressSpace {
    /// Creates a software address space with the requested physical RAM size.
    pub fn new(physical_bytes: usize) -> Result<Self, MemoryError> {
        let page_size =
            usize::try_from(GUEST_PAGE_SIZE).map_err(|_| MemoryError::HostSizeOverflow)?;
        if physical_bytes == 0 || !physical_bytes.is_multiple_of(page_size) {
            return Err(MemoryError::Address(exbawks_types::AddressError::UnalignedRange {
                start: GuestVa::ZERO,
                len: physical_bytes as u64,
            }));
        }

        let page_count =
            u32::try_from(physical_bytes / page_size).map_err(|_| MemoryError::HostSizeOverflow)?;

        Ok(Self {
            physical: RwLock::new(vec![0_u8; physical_bytes].into_boxed_slice()),
            table: PageTable::new(),
            allocator: Mutex::new(PhysicalAllocator { next_page: 0, page_count }),
        })
    }

    /// Maps zeroed contiguous physical pages into a virtual range.
    pub fn map_anonymous(
        &self,
        range: GuestRange,
        permissions: MemoryPermissions,
    ) -> Result<GuestPa, MemoryError> {
        range.require_page_alignment()?;
        let page_count =
            u32::try_from(range.page_count()).map_err(|_| MemoryError::HostSizeOverflow)?;
        let mut allocator = self.allocator.lock();
        let physical_page = allocator.allocate(page_count)?;
        if let Err(error) = self.table.map_ram(range, physical_page, permissions) {
            allocator.rollback_last(physical_page, page_count);
            return Err(error);
        }
        Ok(physical_page.start_pa())
    }

    /// Maps a virtual alias to existing guest physical pages.
    pub fn map_alias(
        &self,
        range: GuestRange,
        physical_start: GuestPa,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        range.require_page_alignment()?;
        if physical_start.page_offset() != 0 {
            return Err(MemoryError::Address(exbawks_types::AddressError::UnalignedRange {
                start: range.start(),
                len: range.len(),
            }));
        }

        let physical_end = u64::from(physical_start.0)
            .checked_add(range.len())
            .ok_or(MemoryError::PhysicalOutOfRange { address: physical_start })?;
        if physical_end > self.physical.read().len() as u64 {
            return Err(MemoryError::PhysicalOutOfRange { address: physical_start });
        }

        self.table.map_ram(range, physical_start.page(), permissions)
    }

    /// Maps a virtual region and copies initial bytes into it.
    pub fn load_region(
        &self,
        address: GuestVa,
        virtual_size: u32,
        initial: &[u8],
        permissions: MemoryPermissions,
    ) -> Result<GuestPa, MemoryError> {
        if initial.len() > virtual_size as usize {
            return Err(MemoryError::HostSizeOverflow);
        }

        let rounded_size = align_up(u64::from(virtual_size), u64::from(GUEST_PAGE_SIZE))?;
        let range = GuestRange::page_aligned(address, rounded_size)?;
        let temporary_permissions = permissions | MemoryPermissions::WRITE;
        let physical = self.map_anonymous(range, temporary_permissions)?;
        self.write(address, initial)?;
        self.table.protect(range, permissions)?;
        Ok(physical)
    }

    /// Changes page permissions for a page-aligned range.
    pub fn protect(
        &self,
        range: GuestRange,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        self.table.protect(range, permissions)
    }

    /// Returns the physical RAM size.
    #[must_use]
    pub fn physical_len(&self) -> usize {
        self.physical.read().len()
    }

    fn access(
        &self,
        access: AccessKind,
        address: GuestVa,
        mut buffer: AccessBuffer<'_>,
    ) -> Result<(), MemoryError> {
        if buffer.is_empty() {
            return Ok(());
        }

        let length = u64::try_from(buffer.len()).map_err(|_| MemoryError::HostSizeOverflow)?;
        let _ = GuestRange::new(address, length)?;

        match &mut buffer {
            AccessBuffer::Read(output) => {
                let physical = self.physical.read();
                validate_access(&self.table, physical.len(), access, address, output.len())?;
                copy_from_guest(&self.table, physical.as_ref(), access, address, output)
            }
            AccessBuffer::Write(input) => {
                let mut physical = self.physical.write();
                validate_access(&self.table, physical.len(), access, address, input.len())?;
                copy_to_guest(&self.table, physical.as_mut(), access, address, input)
            }
        }
    }
}

impl GuestMemory for SoftwareAddressSpace {
    fn read(&self, address: GuestVa, output: &mut [u8]) -> Result<(), MemoryError> {
        self.access(AccessKind::Read, address, AccessBuffer::Read(output))
    }

    fn fetch(&self, address: GuestVa, output: &mut [u8]) -> Result<(), MemoryError> {
        self.access(AccessKind::Execute, address, AccessBuffer::Read(output))
    }

    fn write(&self, address: GuestVa, input: &[u8]) -> Result<(), MemoryError> {
        self.access(AccessKind::Write, address, AccessBuffer::Write(input))
    }

    fn page_table(&self) -> &PageTable {
        &self.table
    }
}

enum AccessBuffer<'a> {
    Read(&'a mut [u8]),
    Write(&'a [u8]),
}

impl AccessBuffer<'_> {
    const fn len(&self) -> usize {
        match self {
            Self::Read(value) => value.len(),
            Self::Write(value) => value.len(),
        }
    }

    const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn validate_access(
    table: &PageTable,
    physical_len: usize,
    access: AccessKind,
    address: GuestVa,
    len: usize,
) -> Result<(), MemoryError> {
    let mut guest = address.0;
    let mut remaining = len;

    while remaining != 0 {
        let current = GuestVa(guest);
        let descriptor = checked_descriptor(table, current, access)?;
        let page_remaining = usize::try_from(GUEST_PAGE_SIZE - current.page_offset())
            .map_err(|_| MemoryError::HostSizeOverflow)?;
        let chunk = cmp::min(page_remaining, remaining);
        let offset = physical_offset(descriptor.physical_page(), current.page_offset())?;
        let end = offset.checked_add(chunk).ok_or(MemoryError::HostSizeOverflow)?;
        if end > physical_len {
            return Err(MemoryError::PhysicalOutOfRange {
                address: descriptor.physical_page().start_pa(),
            });
        }

        remaining -= chunk;
        if remaining != 0 {
            guest = guest
                .checked_add(u32::try_from(chunk).map_err(|_| MemoryError::HostSizeOverflow)?)
                .ok_or(MemoryError::HostSizeOverflow)?;
        }
    }

    Ok(())
}

fn copy_from_guest(
    table: &PageTable,
    physical: &[u8],
    access: AccessKind,
    address: GuestVa,
    output: &mut [u8],
) -> Result<(), MemoryError> {
    let mut guest = address.0;
    let mut destination_offset = 0_usize;

    while destination_offset < output.len() {
        let current = GuestVa(guest);
        let descriptor = checked_descriptor(table, current, access)?;
        let page_remaining = usize::try_from(GUEST_PAGE_SIZE - current.page_offset())
            .map_err(|_| MemoryError::HostSizeOverflow)?;
        let chunk = cmp::min(page_remaining, output.len() - destination_offset);
        let physical_offset = physical_offset(descriptor.physical_page(), current.page_offset())?;
        let physical_end =
            physical_offset.checked_add(chunk).ok_or(MemoryError::HostSizeOverflow)?;
        let source =
            physical.get(physical_offset..physical_end).ok_or(MemoryError::PhysicalOutOfRange {
                address: GuestPa(u32::try_from(physical_offset).unwrap_or(u32::MAX)),
            })?;
        output[destination_offset..destination_offset + chunk].copy_from_slice(source);

        destination_offset += chunk;
        if destination_offset < output.len() {
            guest = guest
                .checked_add(u32::try_from(chunk).map_err(|_| MemoryError::HostSizeOverflow)?)
                .ok_or(MemoryError::HostSizeOverflow)?;
        }
    }

    Ok(())
}

fn copy_to_guest(
    table: &PageTable,
    physical: &mut [u8],
    access: AccessKind,
    address: GuestVa,
    input: &[u8],
) -> Result<(), MemoryError> {
    let mut guest = address.0;
    let mut source_offset = 0_usize;

    while source_offset < input.len() {
        let current = GuestVa(guest);
        let descriptor = checked_descriptor(table, current, access)?;
        let page_remaining = usize::try_from(GUEST_PAGE_SIZE - current.page_offset())
            .map_err(|_| MemoryError::HostSizeOverflow)?;
        let chunk = cmp::min(page_remaining, input.len() - source_offset);
        let physical_offset = physical_offset(descriptor.physical_page(), current.page_offset())?;
        let physical_end =
            physical_offset.checked_add(chunk).ok_or(MemoryError::HostSizeOverflow)?;
        let destination = physical.get_mut(physical_offset..physical_end).ok_or(
            MemoryError::PhysicalOutOfRange {
                address: GuestPa(u32::try_from(physical_offset).unwrap_or(u32::MAX)),
            },
        )?;
        destination.copy_from_slice(&input[source_offset..source_offset + chunk]);

        source_offset += chunk;
        if source_offset < input.len() {
            guest = guest
                .checked_add(u32::try_from(chunk).map_err(|_| MemoryError::HostSizeOverflow)?)
                .ok_or(MemoryError::HostSizeOverflow)?;
        }
    }

    Ok(())
}

fn checked_descriptor(
    table: &PageTable,
    address: GuestVa,
    access: AccessKind,
) -> Result<crate::PageDescriptor, MemoryError> {
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

fn physical_offset(page: GuestPage, page_offset: u32) -> Result<usize, MemoryError> {
    let offset = u64::from(page.0)
        .checked_mul(u64::from(GUEST_PAGE_SIZE))
        .and_then(|base| base.checked_add(u64::from(page_offset)))
        .ok_or(MemoryError::HostSizeOverflow)?;
    usize::try_from(offset).map_err(|_| MemoryError::HostSizeOverflow)
}

fn align_up(value: u64, alignment: u64) -> Result<u64, MemoryError> {
    let mask = alignment - 1;
    value.checked_add(mask).map(|sum| sum & !mask).ok_or(MemoryError::HostSizeOverflow)
}

#[cfg(test)]
mod tests {
    use exbawks_types::{GuestRange, GuestVa, MemoryPermissions};

    use super::*;

    #[test]
    fn aliases_share_physical_bytes() {
        let memory = SoftwareAddressSpace::new(4 * 1024 * 1024).expect("memory is valid");
        let first = GuestRange::page_aligned(GuestVa(0x1000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        let physical = memory
            .map_anonymous(first, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        let second = GuestRange::page_aligned(GuestVa(0x9000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_alias(second, physical, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("alias succeeds");

        memory.write(GuestVa(0x1010), b"exbawks").expect("write succeeds");
        let mut output = [0_u8; 7];
        memory.read(GuestVa(0x9010), &mut output).expect("read succeeds");
        assert_eq!(&output, b"exbawks");
    }

    #[test]
    fn execute_fetch_requires_execute_permission() {
        let memory = SoftwareAddressSpace::new(1024 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory.map_anonymous(range, MemoryPermissions::READ).expect("mapping succeeds");

        let mut byte = [0_u8; 1];
        let error = memory.fetch(GuestVa(0x1000), &mut byte).expect_err("fetch must fail");
        assert!(matches!(error, MemoryError::AccessDenied { .. }));
    }

    #[test]
    fn cross_page_access_preserves_order() {
        let memory = SoftwareAddressSpace::new(1024 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");

        memory.write(GuestVa(0x1FFE), &[1, 2, 3, 4]).expect("write succeeds");
        let mut output = [0_u8; 4];
        memory.read(GuestVa(0x1FFE), &mut output).expect("read succeeds");
        assert_eq!(output, [1, 2, 3, 4]);
    }

    #[test]
    fn access_can_end_at_the_top_of_the_guest_address_space() {
        let memory = SoftwareAddressSpace::new(1024 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0xFFFF_F000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");

        memory.write(GuestVa(0xFFFF_FFFF), &[0xA5]).expect("write succeeds");
        let mut value = [0_u8; 1];
        memory.read(GuestVa(0xFFFF_FFFF), &mut value).expect("read succeeds");
        assert_eq!(value, [0xA5]);
    }

    #[test]
    fn failed_cross_page_write_does_not_change_the_first_page() {
        let memory = SoftwareAddressSpace::new(1024 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        let second = GuestRange::page_aligned(GuestVa(0x2000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory.protect(second, MemoryPermissions::READ).expect("protection succeeds");

        let error = memory.write(GuestVa(0x1FFF), &[0xAA, 0xBB]).expect_err("write must fail");
        assert!(matches!(error, MemoryError::AccessDenied { .. }));

        let mut first = [0_u8; 1];
        memory.read(GuestVa(0x1FFF), &mut first).expect("read succeeds");
        assert_eq!(first, [0]);
    }

    #[test]
    fn failed_anonymous_map_rolls_back_physical_allocation() {
        let memory = SoftwareAddressSpace::new(1024 * 1024).expect("memory is valid");
        let occupied = GuestRange::page_aligned(GuestVa(0x1000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .page_table()
            .map_ram(occupied, GuestPage(100), MemoryPermissions::READ)
            .expect("manual mapping succeeds");

        let error =
            memory.map_anonymous(occupied, MemoryPermissions::READ).expect_err("overlap must fail");
        assert!(matches!(error, MemoryError::AlreadyMapped { .. }));

        let available = GuestRange::page_aligned(GuestVa(0x2000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        let physical =
            memory.map_anonymous(available, MemoryPermissions::READ).expect("mapping succeeds");
        assert_eq!(physical, GuestPa(0));
    }
}
