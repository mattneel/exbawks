use exbawks_platform::virtual_memory::PageProtection;
use exbawks_types::{GUEST_PAGE_SIZE, GuestPa, GuestRange};

use crate::MemoryError;

/// The byte size of a complete 32-bit guest virtual arena.
pub const GUEST_ARENA_SIZE: u64 = 1_u64 << 32;

/// One coherent section view in the future Windows arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestViewPlan {
    /// The guest virtual range that selects the host arena offset.
    pub guest_range: GuestRange,
    /// The offset in the physical RAM section.
    pub section_offset: GuestPa,
    /// The initial host page protection.
    pub protection: PageProtection,
}

/// A validated plan for sparse Windows mappings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsArenaPlan {
    /// A 4 GiB-aligned host arena base.
    pub arena_base: u64,
    /// The physical RAM section size.
    pub physical_bytes: u64,
    /// The sparse section views.
    pub views: Vec<GuestViewPlan>,
}

impl WindowsArenaPlan {
    /// Creates an empty plan.
    pub fn new(arena_base: u64, physical_bytes: u64) -> Result<Self, MemoryError> {
        let arena_end = arena_base.checked_add(GUEST_ARENA_SIZE);
        if arena_base & (GUEST_ARENA_SIZE - 1) != 0 || arena_end.is_none() {
            return Err(MemoryError::InvalidArenaBase { base: arena_base });
        }

        let page_size = u64::from(GUEST_PAGE_SIZE);
        if physical_bytes == 0
            || physical_bytes > GUEST_ARENA_SIZE
            || physical_bytes & (page_size - 1) != 0
        {
            return Err(MemoryError::InvalidPhysicalSize { bytes: physical_bytes });
        }

        Ok(Self { arena_base, physical_bytes, views: Vec::new() })
    }

    /// Adds one validated view.
    pub fn push(&mut self, view: GuestViewPlan) -> Result<(), MemoryError> {
        view.guest_range.require_page_alignment()?;
        if view.section_offset.page_offset() != 0 {
            return Err(MemoryError::UnalignedPhysicalAddress { address: view.section_offset });
        }

        let end = u64::from(view.section_offset.0)
            .checked_add(view.guest_range.len())
            .ok_or(MemoryError::PhysicalOutOfRange { address: view.section_offset })?;
        if end > self.physical_bytes {
            return Err(MemoryError::PhysicalOutOfRange { address: view.section_offset });
        }

        let overlaps = self.views.iter().any(|current| {
            u64::from(view.guest_range.start().0) < current.guest_range.end_exclusive()
                && u64::from(current.guest_range.start().0) < view.guest_range.end_exclusive()
        });
        if overlaps {
            return Err(MemoryError::ArenaViewOverlap { address: view.guest_range.start() });
        }

        self.views.push(view);
        Ok(())
    }

    /// Returns the host address for a guest virtual address.
    #[must_use]
    pub const fn host_address(&self, guest: exbawks_types::GuestVa) -> u64 {
        self.arena_base + guest.0 as u64
    }
}

#[cfg(test)]
mod tests {
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, GuestVa};

    use super::*;

    #[test]
    fn plan_rejects_an_unaligned_arena() {
        let error =
            WindowsArenaPlan::new(0x1_0000, 64 * 1024 * 1024).expect_err("arena must be aligned");
        assert!(matches!(error, MemoryError::InvalidArenaBase { .. }));
    }

    #[test]
    fn plan_rejects_overlapping_views() {
        let mut plan =
            WindowsArenaPlan::new(0x1_0000_0000, 64 * 1024 * 1024).expect("plan is valid");
        let first = GuestViewPlan {
            guest_range: GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
                .expect("range is valid"),
            section_offset: GuestPa(0),
            protection: PageProtection::ReadWrite,
        };
        plan.push(first).expect("first view succeeds");

        let second = GuestViewPlan {
            guest_range: GuestRange::page_aligned(GuestVa(0x2000), u64::from(GUEST_PAGE_SIZE))
                .expect("range is valid"),
            section_offset: GuestPa(GUEST_PAGE_SIZE),
            protection: PageProtection::ReadOnly,
        };
        let error = plan.push(second).expect_err("overlap must fail");
        assert!(matches!(error, MemoryError::ArenaViewOverlap { .. }));
    }
}
