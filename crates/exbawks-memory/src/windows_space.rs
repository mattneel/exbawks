use std::collections::BTreeMap;

use exbawks_platform::virtual_memory::{MappedView, PageProtection, PagefileSection, Placeholder};
use exbawks_types::{AccessKind, GUEST_PAGE_SIZE, GuestPa, GuestRange, GuestVa, MemoryPermissions};
use parking_lot::{Mutex, RwLock};

use crate::access::{AccessBuffer, align_up, physical_offset, walk_pages};
use crate::allocator::PhysicalAllocator;
use crate::{GUEST_ARENA_SIZE, GuestArena, GuestMemory, MemoryError, PageTable};

/// The sparse Windows implementation of the guest address space.
///
/// One pagefile-backed section holds guest physical RAM. Guest mappings
/// replace arena placeholders with coherent section views, and every view is
/// recorded in the sidecar page table.
#[derive(Debug)]
pub struct WindowsAddressSpace {
    arena_base: u64,
    section: PagefileSection,
    physical_bytes: usize,
    table: PageTable,
    allocator: Mutex<PhysicalAllocator>,
    regions: RwLock<RegionMap>,
}

/// Placeholder and view bookkeeping for one guest arena.
#[derive(Debug)]
struct RegionMap {
    /// Free placeholders keyed by guest offset.
    free: BTreeMap<u64, Placeholder>,
    /// Live section views keyed by guest start address.
    views: BTreeMap<u64, MappedView>,
}

impl WindowsAddressSpace {
    /// Creates a Windows address space with the requested physical RAM size.
    pub fn new(physical_bytes: usize) -> Result<Self, MemoryError> {
        let page_size =
            usize::try_from(GUEST_PAGE_SIZE).map_err(|_| MemoryError::HostSizeOverflow)?;
        if physical_bytes == 0 || !physical_bytes.is_multiple_of(page_size) {
            return Err(MemoryError::Address(exbawks_types::AddressError::UnalignedRange {
                start: GuestVa::ZERO,
                len: physical_bytes as u64,
            }));
        }
        if physical_bytes as u64 > GUEST_ARENA_SIZE {
            return Err(MemoryError::InvalidPhysicalSize { bytes: physical_bytes as u64 });
        }

        let page_count =
            u32::try_from(physical_bytes / page_size).map_err(|_| MemoryError::HostSizeOverflow)?;

        let arena = GuestArena::reserve()?;
        let arena_base = arena.base();
        let section = PagefileSection::new(physical_bytes)?;

        let mut free = BTreeMap::new();
        free.insert(0, arena.into_placeholder());

        Ok(Self {
            arena_base,
            section,
            physical_bytes,
            table: PageTable::new(),
            allocator: Mutex::new(PhysicalAllocator::new(page_count)),
            regions: RwLock::new(RegionMap { free, views: BTreeMap::new() }),
        })
    }

    /// Returns the host arena base.
    #[must_use]
    pub const fn arena_base(&self) -> u64 {
        self.arena_base
    }

    /// Returns the host address for one guest virtual address.
    #[must_use]
    pub const fn host_address(&self, guest: GuestVa) -> u64 {
        self.arena_base + guest.0 as u64
    }

    /// Returns the physical RAM size.
    #[must_use]
    pub const fn physical_len(&self) -> usize {
        self.physical_bytes
    }

    /// Returns the sidecar page table.
    #[must_use]
    pub const fn page_table(&self) -> &PageTable {
        &self.table
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
        if let Err(error) = self.map_view(range, physical_page.start_pa(), permissions) {
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

        self.map_view(range, physical_start, permissions)
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

    /// Unmaps one complete mapped view and restores its placeholder.
    pub fn unmap(&self, range: GuestRange) -> Result<(), MemoryError> {
        range.require_page_alignment()?;
        let start = u64::from(range.start().0);
        let mut regions = self.regions.write();

        let matches =
            regions.views.get(&start).is_some_and(|view| view.len() as u64 == range.len());
        if !matches {
            return Err(MemoryError::Unmapped { address: range.start(), access: AccessKind::Read });
        }

        let view = regions.views.remove(&start).expect("presence was checked above");
        match view.unmap_restore() {
            Ok(placeholder) => {
                regions.insert_free(start, placeholder);
                self.table.unmap(range)?;
                Ok(())
            }
            Err(restore) => {
                regions.views.insert(start, restore.view);
                Err(restore.error.into())
            }
        }
    }

    /// Maps one section view and records it in the sidecar page table.
    fn map_view(
        &self,
        range: GuestRange,
        physical_start: GuestPa,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        let physical_end = u64::from(physical_start.0)
            .checked_add(range.len())
            .ok_or(MemoryError::PhysicalOutOfRange { address: physical_start })?;
        if physical_end > self.physical_bytes as u64 {
            return Err(MemoryError::PhysicalOutOfRange { address: physical_start });
        }

        let mut regions = self.regions.write();

        for page in range.pages() {
            if self.table.get(page).is_valid() {
                return Err(MemoryError::AlreadyMapped { address: page.start_va() });
            }
        }

        let start = u64::from(range.start().0);
        let placeholder = regions.carve(start, range.len())?;

        // Arena views always stay host read-write. Guest permissions live in
        // the sidecar page table, which every checked access consults, so
        // both backends enforce identical rules. Host protection tightening
        // arrives with write-fault code invalidation.
        let view = match self.section.map_replace(
            placeholder,
            u64::from(physical_start.0),
            PageProtection::ReadWrite,
        ) {
            Ok(view) => view,
            Err(replace) => {
                regions.insert_free(start, replace.placeholder);
                return Err(replace.error.into());
            }
        };

        if let Err(error) = self.table.map_ram(range, physical_start.page(), permissions) {
            match view.unmap_restore() {
                Ok(placeholder) => regions.insert_free(start, placeholder),
                Err(restore) => {
                    regions.views.insert(start, restore.view);
                }
            }
            return Err(error);
        }

        regions.views.insert(start, view);
        Ok(())
    }

    /// Performs one validated access through the mapped views.
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

        let regions = self.regions.read();
        walk_pages(&self.table, access, address, buffer.len(), |current, descriptor, _, chunk| {
            let offset = physical_offset(descriptor.physical_page(), current.page_offset())?;
            let end = offset.checked_add(chunk).ok_or(MemoryError::HostSizeOverflow)?;
            if end > self.physical_bytes {
                return Err(MemoryError::PhysicalOutOfRange {
                    address: descriptor.physical_page().start_pa(),
                });
            }
            if regions.view_at(u64::from(current.0)).is_none() {
                return Err(MemoryError::Unmapped { address: current, access });
            }
            Ok(())
        })?;

        match &mut buffer {
            AccessBuffer::Read(output) => {
                let len = output.len();
                walk_pages(&self.table, access, address, len, |current, _, offset, chunk| {
                    let (view, view_offset) = regions
                        .view_at(u64::from(current.0))
                        .ok_or(MemoryError::Unmapped { address: current, access })?;
                    view.read_at(view_offset, &mut output[offset..offset + chunk])?;
                    Ok(())
                })
            }
            AccessBuffer::Write(input) => {
                let len = input.len();
                walk_pages(&self.table, access, address, len, |current, _, offset, chunk| {
                    let (view, view_offset) = regions
                        .view_at(u64::from(current.0))
                        .ok_or(MemoryError::Unmapped { address: current, access })?;
                    view.write_at(view_offset, &input[offset..offset + chunk])?;
                    Ok(())
                })
            }
        }
    }
}

impl GuestMemory for WindowsAddressSpace {
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

impl RegionMap {
    /// Returns the view and view offset that contain one guest position.
    fn view_at(&self, guest_offset: u64) -> Option<(&MappedView, usize)> {
        let (&start, view) = self.views.range(..=guest_offset).next_back()?;
        let offset = guest_offset - start;
        if offset < view.len() as u64 { Some((view, usize::try_from(offset).ok()?)) } else { None }
    }
    /// Removes the exact free range and returns its placeholder.
    fn carve(&mut self, start: u64, len: u64) -> Result<Placeholder, MemoryError> {
        let address = GuestVa(start as u32);
        let candidate = self
            .free
            .range(..=start)
            .next_back()
            .map(|(&candidate_start, candidate)| (candidate_start, candidate.len() as u64));
        let Some((candidate_start, candidate_len)) = candidate else {
            return Err(MemoryError::AlreadyMapped { address });
        };
        if candidate_start + candidate_len < start + len {
            return Err(MemoryError::AlreadyMapped { address });
        }

        let head_len =
            usize::try_from(start - candidate_start).map_err(|_| MemoryError::HostSizeOverflow)?;
        let target_len = usize::try_from(len).map_err(|_| MemoryError::HostSizeOverflow)?;

        let mut candidate =
            self.free.remove(&candidate_start).expect("the candidate key was found above");

        let mut target = if head_len > 0 {
            match candidate.split_off(head_len) {
                Ok(target) => {
                    self.free.insert(candidate_start, candidate);
                    target
                }
                Err(error) => {
                    self.free.insert(candidate_start, candidate);
                    return Err(error.into());
                }
            }
        } else {
            candidate
        };

        if target.len() > target_len {
            match target.split_off(target_len) {
                Ok(tail) => {
                    self.free.insert(start + len, tail);
                }
                Err(error) => {
                    self.insert_free(start, target);
                    return Err(error.into());
                }
            }
        }

        Ok(target)
    }

    /// Inserts a free placeholder and coalesces adjacent free neighbors.
    fn insert_free(&mut self, offset: u64, placeholder: Placeholder) {
        let mut offset = offset;
        let mut placeholder = placeholder;

        let previous = self
            .free
            .range(..offset)
            .next_back()
            .map(|(&previous_start, previous)| (previous_start, previous.len() as u64));
        if let Some((previous_start, previous_len)) = previous
            && previous_start + previous_len == offset
        {
            let mut previous =
                self.free.remove(&previous_start).expect("the previous key was found above");
            match previous.coalesce_with(placeholder) {
                Ok(()) => {
                    offset = previous_start;
                    placeholder = previous;
                }
                Err(error) => {
                    self.free.insert(previous_start, previous);
                    placeholder = error.next;
                }
            }
        }

        let end = offset + placeholder.len() as u64;
        if self.free.contains_key(&end) {
            let next = self.free.remove(&end).expect("the next key was found above");
            if let Err(error) = placeholder.coalesce_with(next) {
                self.free.insert(end, error.next);
            }
        }

        self.free.insert(offset, placeholder);
    }
}

#[cfg(test)]
mod tests {
    use exbawks_types::GuestPage;

    use super::*;

    #[cfg(windows)]
    const PAGE: u64 = GUEST_PAGE_SIZE as u64;
    #[cfg(windows)]
    const READ_WRITE: MemoryPermissions = MemoryPermissions::READ.union(MemoryPermissions::WRITE);

    #[cfg(windows)]
    fn space() -> WindowsAddressSpace {
        WindowsAddressSpace::new(4 * 1024 * 1024).expect("space is valid")
    }

    #[cfg(windows)]
    #[test]
    fn alias_write_appears_through_both_aliases() {
        let space = space();
        let first = GuestRange::page_aligned(GuestVa(0x1000), PAGE).expect("range is valid");
        let physical = space.map_anonymous(first, READ_WRITE).expect("mapping succeeds");
        let second = GuestRange::page_aligned(GuestVa(0x9000), PAGE).expect("range is valid");
        space.map_alias(second, physical, READ_WRITE).expect("alias succeeds");

        let regions = space.regions.read();
        let first_view = regions.views.get(&0x1000).expect("first view exists");
        let second_view = regions.views.get(&0x9000).expect("second view exists");
        first_view.write_at(0x10, b"exbawks").expect("write succeeds");
        let mut output = [0_u8; 7];
        second_view.read_at(0x10, &mut output).expect("read succeeds");
        assert_eq!(&output, b"exbawks");

        assert_eq!(space.page_table().get(GuestPage(1)).physical_page(), GuestPage(0));
        assert_eq!(space.page_table().get(GuestPage(9)).physical_page(), GuestPage(0));
    }

    #[cfg(windows)]
    #[test]
    fn unmap_one_alias_keeps_the_other() {
        let space = space();
        let first = GuestRange::page_aligned(GuestVa(0x1000), PAGE).expect("range is valid");
        let physical = space.map_anonymous(first, READ_WRITE).expect("mapping succeeds");
        let second = GuestRange::page_aligned(GuestVa(0x9000), PAGE).expect("range is valid");
        space.map_alias(second, physical, READ_WRITE).expect("alias succeeds");

        {
            let regions = space.regions.read();
            let second_view = regions.views.get(&0x9000).expect("second view exists");
            second_view.write_at(0, &[0xA5]).expect("write succeeds");
        }

        space.unmap(first).expect("unmap succeeds");
        assert!(!space.page_table().get(GuestPage(1)).is_valid());
        assert!(space.page_table().get(GuestPage(9)).is_valid());

        let regions = space.regions.read();
        assert!(!regions.views.contains_key(&0x1000));
        let second_view = regions.views.get(&0x9000).expect("second view remains");
        let mut output = [0_u8; 1];
        second_view.read_at(0, &mut output).expect("read succeeds");
        assert_eq!(output, [0xA5]);
        drop(regions);

        space.map_alias(first, physical, READ_WRITE).expect("the freed range maps again");
    }

    #[cfg(windows)]
    #[test]
    fn unmap_restores_coalesced_free_space() {
        let space = space();
        let middle = GuestRange::page_aligned(GuestVa(0x2000), PAGE).expect("range is valid");
        space.map_anonymous(middle, READ_WRITE).expect("mapping succeeds");
        space.unmap(middle).expect("unmap succeeds");

        let spanning = GuestRange::page_aligned(GuestVa(0x1000), 3 * PAGE).expect("range is valid");
        space.map_anonymous(spanning, READ_WRITE).expect("a spanning map succeeds");

        let regions = space.regions.read();
        assert_eq!(regions.free.len(), 2);
        assert_eq!(regions.views.len(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn overlapping_view_plans_are_rejected() {
        let space = space();
        let first = GuestRange::page_aligned(GuestVa(0x1000), 2 * PAGE).expect("range is valid");
        let physical = space.map_anonymous(first, READ_WRITE).expect("mapping succeeds");

        let overlap = GuestRange::page_aligned(GuestVa(0x2000), PAGE).expect("range is valid");
        let error = space.map_alias(overlap, physical, READ_WRITE).expect_err("overlap must fail");
        assert!(matches!(error, MemoryError::AlreadyMapped { .. }));

        let free = GuestRange::page_aligned(GuestVa(0x3000), PAGE).expect("range is valid");
        space.map_alias(free, physical, READ_WRITE).expect("an open range still maps");
    }

    #[cfg(windows)]
    #[test]
    fn section_ranges_outside_physical_ram_are_rejected() {
        let space = space();
        let range = GuestRange::page_aligned(GuestVa(0x1000), PAGE).expect("range is valid");

        let outside = GuestPa(4 * 1024 * 1024);
        let error =
            space.map_alias(range, outside, READ_WRITE).expect_err("outside offset must fail");
        assert!(matches!(error, MemoryError::PhysicalOutOfRange { .. }));

        let straddling = GuestPa(4 * 1024 * 1024 - GUEST_PAGE_SIZE);
        let long = GuestRange::page_aligned(GuestVa(0x1000), 2 * PAGE).expect("range is valid");
        let error =
            space.map_alias(long, straddling, READ_WRITE).expect_err("straddling range must fail");
        assert!(matches!(error, MemoryError::PhysicalOutOfRange { .. }));
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_space_reports_an_unsupported_host() {
        let error = WindowsAddressSpace::new(4 * 1024 * 1024).expect_err("construction must fail");
        assert!(matches!(error, MemoryError::Platform(_)));
    }
}
