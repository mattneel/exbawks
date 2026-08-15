use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};

use exbawks_types::{GUEST_PAGE_COUNT, GUEST_PAGE_SIZE, GuestPage, GuestRange, MemoryPermissions};
use parking_lot::Mutex;

use crate::MemoryError;

const KIND_SHIFT: u32 = 0;
const KIND_MASK: u64 = 0b111;
const PERMISSIONS_SHIFT: u32 = 3;
const PERMISSIONS_MASK: u64 = 0b111;
const PHYSICAL_PAGE_SHIFT: u32 = 8;
const PHYSICAL_PAGE_MASK: u64 = (1 << 20) - 1;
const AUX_SHIFT: u32 = 28;
const AUX_MASK: u64 = (1 << 16) - 1;
const GENERATION_SHIFT: u32 = 44;
const GENERATION_MASK: u64 = (1 << 16) - 1;
const WATCH_SHIFT: u32 = 60;
const WATCH_MASK: u64 = 0b111;
const VALID_BIT: u64 = 1 << 63;

/// The backing type of one guest page.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageKind {
    /// No mapping exists.
    #[default]
    Unmapped = 0,
    /// The page maps guest physical RAM.
    Ram = 1,
    /// The page dispatches to an MMIO handler.
    Mmio = 2,
    /// The page is reserved without accessible data.
    Reserved = 3,
}

impl PageKind {
    const fn from_raw(value: u8) -> Self {
        match value {
            1 => Self::Ram,
            2 => Self::Mmio,
            3 => Self::Reserved,
            _ => Self::Unmapped,
        }
    }
}

/// Debug watch flags for one guest page.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WatchFlags {
    /// Watch reads.
    pub read: bool,
    /// Watch writes.
    pub write: bool,
    /// Watch instruction fetches.
    pub execute: bool,
}

impl WatchFlags {
    const fn to_raw(self) -> u8 {
        (self.read as u8) | ((self.write as u8) << 1) | ((self.execute as u8) << 2)
    }

    const fn from_raw(value: u8) -> Self {
        Self { read: value & 1 != 0, write: value & 2 != 0, execute: value & 4 != 0 }
    }
}

/// Packed metadata for one guest virtual page.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct PageDescriptor(u64);

impl PageDescriptor {
    /// Creates a guest RAM page descriptor.
    #[must_use]
    pub const fn ram(
        physical_page: GuestPage,
        permissions: MemoryPermissions,
        generation: u16,
    ) -> Self {
        Self::pack(PageKind::Ram, permissions, physical_page.0, 0, generation, WatchFlags::NONE)
    }

    /// Creates an MMIO page descriptor.
    #[must_use]
    pub const fn mmio(handler_id: u16, permissions: MemoryPermissions, generation: u16) -> Self {
        Self::pack(PageKind::Mmio, permissions, 0, handler_id, generation, WatchFlags::NONE)
    }

    /// Creates a reserved page descriptor.
    #[must_use]
    pub const fn reserved(generation: u16) -> Self {
        Self::pack(
            PageKind::Reserved,
            MemoryPermissions::empty(),
            0,
            0,
            generation,
            WatchFlags::NONE,
        )
    }

    const fn pack(
        kind: PageKind,
        permissions: MemoryPermissions,
        physical_page: u32,
        aux: u16,
        generation: u16,
        watch: WatchFlags,
    ) -> Self {
        let mut raw = VALID_BIT;
        raw |= (kind as u64) << KIND_SHIFT;
        raw |= ((permissions.bits() as u64) & PERMISSIONS_MASK) << PERMISSIONS_SHIFT;
        raw |= ((physical_page as u64) & PHYSICAL_PAGE_MASK) << PHYSICAL_PAGE_SHIFT;
        raw |= ((aux as u64) & AUX_MASK) << AUX_SHIFT;
        raw |= ((generation as u64) & GENERATION_MASK) << GENERATION_SHIFT;
        raw |= ((watch.to_raw() as u64) & WATCH_MASK) << WATCH_SHIFT;
        Self(raw)
    }

    /// Returns true when the descriptor contains a mapping.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.0 & VALID_BIT != 0
    }

    /// Returns the page kind.
    #[must_use]
    pub const fn kind(self) -> PageKind {
        PageKind::from_raw(((self.0 >> KIND_SHIFT) & KIND_MASK) as u8)
    }

    /// Returns the guest page permissions.
    #[must_use]
    pub const fn permissions(self) -> MemoryPermissions {
        MemoryPermissions::from_bits_retain(
            ((self.0 >> PERMISSIONS_SHIFT) & PERMISSIONS_MASK) as u8,
        )
    }

    /// Returns the mapped guest physical page.
    #[must_use]
    pub const fn physical_page(self) -> GuestPage {
        GuestPage(((self.0 >> PHYSICAL_PAGE_SHIFT) & PHYSICAL_PAGE_MASK) as u32)
    }

    /// Returns the MMIO handler identifier.
    #[must_use]
    pub const fn handler_id(self) -> u16 {
        ((self.0 >> AUX_SHIFT) & AUX_MASK) as u16
    }

    /// Returns the physical code generation.
    #[must_use]
    pub const fn generation(self) -> u16 {
        ((self.0 >> GENERATION_SHIFT) & GENERATION_MASK) as u16
    }

    /// Returns debug watch flags.
    #[must_use]
    pub const fn watch(self) -> WatchFlags {
        WatchFlags::from_raw(((self.0 >> WATCH_SHIFT) & WATCH_MASK) as u8)
    }

    /// Returns a copy with new permissions.
    #[must_use]
    pub const fn with_permissions(self, permissions: MemoryPermissions) -> Self {
        let raw = self.0 & !(PERMISSIONS_MASK << PERMISSIONS_SHIFT);
        Self(raw | (((permissions.bits() as u64) & PERMISSIONS_MASK) << PERMISSIONS_SHIFT))
    }

    /// Returns a copy with a new generation.
    #[must_use]
    pub const fn with_generation(self, generation: u16) -> Self {
        let raw = self.0 & !(GENERATION_MASK << GENERATION_SHIFT);
        Self(raw | ((generation as u64) << GENERATION_SHIFT))
    }

    /// Returns a copy with new watch flags.
    #[must_use]
    pub const fn with_watch(self, watch: WatchFlags) -> Self {
        let raw = self.0 & !(WATCH_MASK << WATCH_SHIFT);
        Self(raw | ((watch.to_raw() as u64) << WATCH_SHIFT))
    }
}

impl WatchFlags {
    const NONE: Self = Self { read: false, write: false, execute: false };
}

/// Atomic metadata for all pages in the 32-bit guest address space.
#[derive(Debug)]
pub struct PageTable {
    entries: Box<[AtomicU64]>,
    physical_generations: Box<[AtomicU16]>,
    mutation_lock: Mutex<()>,
}

impl PageTable {
    /// Creates an empty page table.
    #[must_use]
    pub fn new() -> Self {
        let mut entries = Vec::with_capacity(GUEST_PAGE_COUNT);
        entries.resize_with(GUEST_PAGE_COUNT, || AtomicU64::new(0));

        let mut physical_generations = Vec::with_capacity(GUEST_PAGE_COUNT);
        physical_generations.resize_with(GUEST_PAGE_COUNT, || AtomicU16::new(0));

        Self {
            entries: entries.into_boxed_slice(),
            physical_generations: physical_generations.into_boxed_slice(),
            mutation_lock: Mutex::new(()),
        }
    }

    /// Loads one descriptor.
    #[must_use]
    pub fn get(&self, page: GuestPage) -> PageDescriptor {
        self.entries
            .get(page.index())
            .map(|entry| PageDescriptor(entry.load(Ordering::Acquire)))
            .unwrap_or_default()
    }

    fn set(&self, page: GuestPage, descriptor: PageDescriptor) {
        let entry =
            self.entries.get(page.index()).expect("validated guest page must fit the page table");
        entry.store(descriptor.0, Ordering::Release);
    }

    /// Maps a page-aligned virtual range to contiguous physical pages.
    pub fn map_ram(
        &self,
        range: GuestRange,
        physical_start: GuestPage,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        range.require_page_alignment()?;
        let page_count =
            u32::try_from(range.page_count()).map_err(|_| MemoryError::HostSizeOverflow)?;
        let physical_end = physical_start
            .0
            .checked_add(page_count)
            .ok_or(MemoryError::PhysicalOutOfRange { address: physical_start.start_pa() })?;
        if physical_end > GUEST_PAGE_COUNT as u32 {
            return Err(MemoryError::PhysicalOutOfRange { address: physical_start.start_pa() });
        }

        let _mutation = self.mutation_lock.lock();

        for page in range.pages() {
            if self.get(page).is_valid() {
                return Err(MemoryError::AlreadyMapped { address: page.start_va() });
            }
        }

        for (offset, page) in range.pages().enumerate() {
            let offset = u32::try_from(offset).map_err(|_| MemoryError::HostSizeOverflow)?;
            let physical = GuestPage(
                physical_start.0.checked_add(offset).ok_or(MemoryError::HostSizeOverflow)?,
            );
            let generation = self
                .physical_generation(physical)
                .ok_or(MemoryError::PhysicalOutOfRange { address: physical.start_pa() })?;
            self.set(page, PageDescriptor::ram(physical, permissions, generation));
        }

        Ok(())
    }

    /// Maps a page-aligned virtual range to one MMIO handler.
    pub fn map_mmio(
        &self,
        range: GuestRange,
        handler_id: u16,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        range.require_page_alignment()?;
        let _mutation = self.mutation_lock.lock();

        for page in range.pages() {
            if self.get(page).is_valid() {
                return Err(MemoryError::AlreadyMapped { address: page.start_va() });
            }
        }

        for page in range.pages() {
            self.set(page, PageDescriptor::mmio(handler_id, permissions, 0));
        }

        Ok(())
    }

    /// Reserves a page-aligned virtual range without access permissions.
    pub fn reserve(&self, range: GuestRange) -> Result<(), MemoryError> {
        range.require_page_alignment()?;
        let _mutation = self.mutation_lock.lock();

        for page in range.pages() {
            if self.get(page).is_valid() {
                return Err(MemoryError::AlreadyMapped { address: page.start_va() });
            }
        }

        for page in range.pages() {
            self.set(page, PageDescriptor::reserved(0));
        }

        Ok(())
    }

    /// Changes watch flags for one mapped guest page.
    pub fn set_watch(&self, page: GuestPage, watch: WatchFlags) -> Result<(), MemoryError> {
        let _mutation = self.mutation_lock.lock();
        let descriptor = self.get(page);
        if !descriptor.is_valid() {
            return Err(MemoryError::Unmapped {
                address: page.start_va(),
                access: exbawks_types::AccessKind::Read,
            });
        }

        self.set(page, descriptor.with_watch(watch));
        Ok(())
    }

    /// Changes permissions for an existing page-aligned range.
    pub fn protect(
        &self,
        range: GuestRange,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        range.require_page_alignment()?;
        let _mutation = self.mutation_lock.lock();

        for page in range.pages() {
            let current = self.get(page);
            if !current.is_valid() {
                return Err(MemoryError::Unmapped {
                    address: page.start_va(),
                    access: exbawks_types::AccessKind::Read,
                });
            }
        }

        for page in range.pages() {
            let current = self.get(page);
            self.set(page, current.with_permissions(permissions));
        }

        Ok(())
    }

    /// Removes mappings from a page-aligned range.
    pub fn unmap(&self, range: GuestRange) -> Result<(), MemoryError> {
        range.require_page_alignment()?;
        let _mutation = self.mutation_lock.lock();
        for page in range.pages() {
            self.set(page, PageDescriptor::default());
        }
        Ok(())
    }

    /// Returns the current generation for one guest physical page.
    #[must_use]
    pub fn physical_generation(&self, physical_page: GuestPage) -> Option<u16> {
        self.physical_generations
            .get(physical_page.index())
            .map(|generation| generation.load(Ordering::Acquire))
    }

    /// Increments the generation of every virtual page that maps one physical page.
    pub fn bump_physical_generation(&self, physical_page: GuestPage) -> Option<u16> {
        let _mutation = self.mutation_lock.lock();
        let generation = self.physical_generations.get(physical_page.index())?;
        let next_generation = generation.load(Ordering::Acquire).wrapping_add(1);
        generation.store(next_generation, Ordering::Release);

        for entry in &self.entries {
            let raw = entry.load(Ordering::Acquire);
            let descriptor = PageDescriptor(raw);
            if descriptor.kind() != PageKind::Ram || descriptor.physical_page() != physical_page {
                continue;
            }

            entry.store(descriptor.with_generation(next_generation).0, Ordering::Release);
        }

        Some(next_generation)
    }

    /// Returns the metadata memory size in bytes.
    #[must_use]
    pub const fn byte_len() -> usize {
        GUEST_PAGE_COUNT * (std::mem::size_of::<AtomicU64>() + std::mem::size_of::<AtomicU16>())
    }

    /// Returns the fixed guest page size.
    #[must_use]
    pub const fn page_size() -> u32 {
        GUEST_PAGE_SIZE
    }
}

impl Default for PageTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use exbawks_types::{GuestVa, MemoryPermissions};

    use super::*;

    #[test]
    fn descriptor_round_trip_preserves_fields() {
        let descriptor = PageDescriptor::ram(
            GuestPage(0x12345),
            MemoryPermissions::READ | MemoryPermissions::EXECUTE,
            77,
        )
        .with_watch(WatchFlags { read: true, write: false, execute: true });

        assert_eq!(descriptor.kind(), PageKind::Ram);
        assert_eq!(descriptor.physical_page(), GuestPage(0x12345));
        assert_eq!(descriptor.generation(), 77);
        assert!(descriptor.permissions().contains(MemoryPermissions::EXECUTE));
        assert_eq!(descriptor.watch(), WatchFlags { read: true, write: false, execute: true });
    }

    #[test]
    fn undefined_permission_bits_cannot_corrupt_other_fields() {
        let poisoned = MemoryPermissions::from_bits_retain(0xFF);
        let descriptor =
            PageDescriptor::ram(GuestPage(0x12345), poisoned, 7).with_permissions(poisoned);

        assert_eq!(descriptor.kind(), PageKind::Ram);
        assert_eq!(descriptor.physical_page(), GuestPage(0x12345));
        assert_eq!(descriptor.generation(), 7);
        assert_eq!(descriptor.permissions().bits(), 0b111);
    }

    #[test]
    fn table_maps_contiguous_physical_pages() {
        let table = PageTable::new();
        let range = GuestRange::page_aligned(GuestVa(0x2000), 2 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is aligned");
        table.map_ram(range, GuestPage(7), MemoryPermissions::READ).expect("mapping succeeds");

        assert_eq!(table.get(GuestPage(2)).physical_page(), GuestPage(7));
        assert_eq!(table.get(GuestPage(3)).physical_page(), GuestPage(8));
    }

    #[test]
    fn failed_map_does_not_change_earlier_pages() {
        let table = PageTable::new();
        let occupied = GuestRange::page_aligned(GuestVa(0x2000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is aligned");
        table
            .map_ram(occupied, GuestPage(9), MemoryPermissions::READ)
            .expect("initial mapping succeeds");

        let overlap = GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is aligned");
        let error = table
            .map_ram(overlap, GuestPage(20), MemoryPermissions::READ)
            .expect_err("overlap must fail");

        assert!(matches!(error, MemoryError::AlreadyMapped { .. }));
        assert!(!table.get(GuestPage(1)).is_valid());
        assert_eq!(table.get(GuestPage(2)).physical_page(), GuestPage(9));
    }

    #[test]
    fn failed_protect_does_not_change_earlier_pages() {
        let table = PageTable::new();
        let mapped = GuestRange::page_aligned(GuestVa(0x1000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is aligned");
        table.map_ram(mapped, GuestPage(4), MemoryPermissions::READ).expect("mapping succeeds");

        let partially_unmapped =
            GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
                .expect("range is aligned");
        let error = table
            .protect(partially_unmapped, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect_err("protection must fail");

        assert!(matches!(error, MemoryError::Unmapped { .. }));
        assert_eq!(table.get(GuestPage(1)).permissions(), MemoryPermissions::READ);
    }

    #[test]
    fn mmio_mapping_records_handler_and_permissions() {
        let table = PageTable::new();
        let range = GuestRange::page_aligned(GuestVa(0x4000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is aligned");
        table
            .map_mmio(range, 12, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");

        let descriptor = table.get(GuestPage(4));
        assert_eq!(descriptor.kind(), PageKind::Mmio);
        assert_eq!(descriptor.handler_id(), 12);
        assert!(descriptor.permissions().contains(MemoryPermissions::WRITE));
    }

    #[test]
    fn reserved_mapping_has_no_access() {
        let table = PageTable::new();
        let range = GuestRange::page_aligned(GuestVa(0x5000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is aligned");
        table.reserve(range).expect("reservation succeeds");

        let descriptor = table.get(GuestPage(5));
        assert_eq!(descriptor.kind(), PageKind::Reserved);
        assert!(descriptor.permissions().is_empty());
    }

    #[test]
    fn invalid_page_lookup_returns_an_unmapped_descriptor() {
        let table = PageTable::new();
        let descriptor = table.get(GuestPage(GUEST_PAGE_COUNT as u32));
        assert!(!descriptor.is_valid());
    }

    #[test]
    fn new_alias_inherits_physical_generation() {
        let table = PageTable::new();
        let first = GuestRange::page_aligned(GuestVa(0x1000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is aligned");
        table.map_ram(first, GuestPage(7), MemoryPermissions::READ).expect("mapping succeeds");
        assert_eq!(table.bump_physical_generation(GuestPage(7)), Some(1));

        let alias = GuestRange::page_aligned(GuestVa(0x9000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is aligned");
        table.map_ram(alias, GuestPage(7), MemoryPermissions::READ).expect("alias succeeds");

        assert_eq!(table.get(GuestPage(1)).generation(), 1);
        assert_eq!(table.get(GuestPage(9)).generation(), 1);
        assert_eq!(table.physical_generation(GuestPage(7)), Some(1));
    }
}
