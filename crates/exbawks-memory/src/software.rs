use exbawks_types::{AccessKind, GUEST_PAGE_SIZE, GuestPa, GuestRange, GuestVa, MemoryPermissions};
use parking_lot::{Mutex, RwLock};

use crate::access::{AccessBuffer, align_up, physical_offset, walk_pages};
use crate::allocator::PhysicalAllocator;
use crate::{MemoryError, PageTable};

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

/// A deterministic software implementation of the guest address space.
#[derive(Debug)]
pub struct SoftwareAddressSpace {
    /// Guest physical RAM in a page-aligned, address-stable allocation so a
    /// hypervisor tier can map the same bytes the software MMU serves
    /// (ADR 0013).
    physical: RwLock<exbawks_platform::AlignedBuffer>,
    table: PageTable,
    allocator: Mutex<PhysicalAllocator>,
    /// Bumped by every mapping mutation (`map_anonymous`, `map_alias`,
    /// `protect`); a hypervisor tier resynchronizes its guest mappings when
    /// the epoch changes.
    mapping_epoch: std::sync::atomic::AtomicU64,
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
        if physical_bytes as u64 > crate::GUEST_ARENA_SIZE {
            return Err(MemoryError::InvalidPhysicalSize { bytes: physical_bytes as u64 });
        }

        let page_count =
            u32::try_from(physical_bytes / page_size).map_err(|_| MemoryError::HostSizeOverflow)?;

        let physical = exbawks_platform::AlignedBuffer::new_zeroed(physical_bytes)
            .ok_or(MemoryError::InvalidPhysicalSize { bytes: physical_bytes as u64 })?;
        Ok(Self {
            physical: RwLock::new(physical),
            table: PageTable::new(),
            allocator: Mutex::new(PhysicalAllocator::new(page_count)),
            mapping_epoch: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// The stable base address of guest physical RAM.
    ///
    /// The allocation is 4 KiB-aligned and never moves for the lifetime of
    /// this address space, so a hypervisor tier can map it into a partition;
    /// interior bytes stay coherent because both tiers address the same
    /// memory. The pointer must not outlive `self`.
    #[must_use]
    pub fn physical_base_ptr(&self) -> *const u8 {
        self.physical.read().base_ptr()
    }

    /// The current mapping epoch; changes whenever guest mappings mutate.
    #[must_use]
    pub fn mapping_epoch(&self) -> u64 {
        self.mapping_epoch.load(std::sync::atomic::Ordering::Acquire)
    }

    fn bump_mapping_epoch(&self) {
        self.mapping_epoch.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
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
        self.bump_mapping_epoch();
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
            return Err(MemoryError::UnalignedPhysicalAddress { address: physical_start });
        }

        let physical_end = u64::from(physical_start.0)
            .checked_add(range.len())
            .ok_or(MemoryError::PhysicalOutOfRange { address: physical_start })?;
        if physical_end > self.physical.read().len() as u64 {
            return Err(MemoryError::PhysicalOutOfRange { address: physical_start });
        }

        self.table.map_ram(range, physical_start.page(), permissions)?;
        self.bump_mapping_epoch();
        Ok(())
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
        self.protect(range, permissions)?;
        Ok(physical)
    }

    /// Changes page permissions for a page-aligned range.
    pub fn protect(
        &self,
        range: GuestRange,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        // The exclusive physical lock keeps permission changes out of the
        // window between access validation and the byte copies.
        let _physical = self.physical.write();
        self.table.protect(range, permissions)?;
        self.bump_mapping_epoch();
        Ok(())
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
                copy_from_guest(&self.table, &physical, access, address, output)
            }
            AccessBuffer::Write(input) => {
                let mut physical = self.physical.write();
                validate_access(&self.table, physical.len(), access, address, input.len())?;
                copy_to_guest(&self.table, &mut physical, access, address, input)?;
                crate::access::bump_written_generations(&self.table, address, input.len());
                Ok(())
            }
        }
    }
}

// Guest writes bump the physical generation of every covered RAM page (see
// `bump_written_generations`), which is what lets the code cache observe
// self-modifying code from either execution tier.
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

fn validate_access(
    table: &PageTable,
    physical_len: usize,
    access: AccessKind,
    address: GuestVa,
    len: usize,
) -> Result<(), MemoryError> {
    walk_pages(table, access, address, len, |current, descriptor, _, chunk| {
        let offset = physical_offset(descriptor.physical_page(), current.page_offset())?;
        let end = offset.checked_add(chunk).ok_or(MemoryError::HostSizeOverflow)?;
        if end > physical_len {
            return Err(MemoryError::PhysicalOutOfRange {
                address: descriptor.physical_page().start_pa(),
            });
        }
        Ok(())
    })
}

fn copy_from_guest(
    table: &PageTable,
    physical: &[u8],
    access: AccessKind,
    address: GuestVa,
    output: &mut [u8],
) -> Result<(), MemoryError> {
    let len = output.len();
    walk_pages(table, access, address, len, |current, descriptor, buffer_offset, chunk| {
        let physical_offset = physical_offset(descriptor.physical_page(), current.page_offset())?;
        let physical_end =
            physical_offset.checked_add(chunk).ok_or(MemoryError::HostSizeOverflow)?;
        let source =
            physical.get(physical_offset..physical_end).ok_or(MemoryError::PhysicalOutOfRange {
                address: GuestPa(u32::try_from(physical_offset).unwrap_or(u32::MAX)),
            })?;
        output[buffer_offset..buffer_offset + chunk].copy_from_slice(source);
        Ok(())
    })
}

fn copy_to_guest(
    table: &PageTable,
    physical: &mut [u8],
    access: AccessKind,
    address: GuestVa,
    input: &[u8],
) -> Result<(), MemoryError> {
    let len = input.len();
    walk_pages(table, access, address, len, |current, descriptor, buffer_offset, chunk| {
        let physical_offset = physical_offset(descriptor.physical_page(), current.page_offset())?;
        let physical_end =
            physical_offset.checked_add(chunk).ok_or(MemoryError::HostSizeOverflow)?;
        let destination = physical.get_mut(physical_offset..physical_end).ok_or(
            MemoryError::PhysicalOutOfRange {
                address: GuestPa(u32::try_from(physical_offset).unwrap_or(u32::MAX)),
            },
        )?;
        destination.copy_from_slice(&input[buffer_offset..buffer_offset + chunk]);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use exbawks_types::{GuestPage, GuestRange, GuestVa, MemoryPermissions};

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
    fn writes_bump_physical_page_generations() {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        let table = memory.page_table();
        let first = table.get(GuestVa(0x1000).page()).physical_page();
        let second = table.get(GuestVa(0x2000).page()).physical_page();
        let before_first = table.physical_generation(first).expect("generation exists");
        let before_second = table.physical_generation(second).expect("generation exists");

        // A write spanning both pages bumps both generations exactly once.
        memory.write(GuestVa(0x1FFE), &[1, 2, 3, 4]).expect("write succeeds");
        assert_eq!(
            table.physical_generation(first),
            Some(before_first.wrapping_add(1)),
            "the first written page must invalidate"
        );
        assert_eq!(table.physical_generation(second), Some(before_second.wrapping_add(1)));

        // A failed write bumps nothing.
        let error = memory.write(GuestVa(0x2FFF), &[1, 2]).expect_err("write must fail");
        assert!(matches!(error, MemoryError::Unmapped { .. }));
        assert_eq!(table.physical_generation(first), Some(before_first.wrapping_add(1)));

        // Reads bump nothing.
        let mut scratch = [0_u8; 4];
        memory.read(GuestVa(0x1FFE), &mut scratch).expect("read succeeds");
        assert_eq!(table.physical_generation(first), Some(before_first.wrapping_add(1)));
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
