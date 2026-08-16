use exbawks_types::GuestPage;

use crate::MemoryError;

/// A bump allocator for contiguous guest physical pages.
#[derive(Debug)]
pub(crate) struct PhysicalAllocator {
    next_page: u32,
    page_count: u32,
}

impl PhysicalAllocator {
    pub(crate) const fn new(page_count: u32) -> Self {
        Self { next_page: 0, page_count }
    }

    pub(crate) fn allocate(&mut self, requested_pages: u32) -> Result<GuestPage, MemoryError> {
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

    /// The next page the allocator would hand out.
    pub(crate) fn next_page(&self) -> u32 {
        self.next_page
    }

    /// The total number of physical pages.
    pub(crate) fn page_count(&self) -> u32 {
        self.page_count
    }

    pub(crate) fn rollback_last(&mut self, start: GuestPage, page_count: u32) {
        let expected_end =
            start.0.checked_add(page_count).expect("a completed allocation cannot overflow");
        assert_eq!(self.next_page, expected_end, "only the latest allocation can roll back");
        self.next_page = start.0;
    }
}
