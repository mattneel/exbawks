use std::collections::BTreeMap;

use exbawks_types::GuestPage;

use crate::MemoryError;

/// An allocator for contiguous guest physical pages: a bump cursor with a
/// free pool in front of it.
///
/// It began as a pure bump allocator, which was honest until titles started
/// churning: a menu transition allocates and frees full-screen surfaces,
/// and a free that returns nothing to the pool exhausts sixty-four
/// megabytes in about a minute of menus — after which the title's
/// out-of-memory error paths run, and a 2003 error path is where the bugs
/// live. Freed runs are coalesced and reused first; the cursor only grows
/// when no freed run fits.
#[derive(Debug)]
pub(crate) struct PhysicalAllocator {
    next_page: u32,
    page_count: u32,
    /// Freed runs by starting page, coalesced with their neighbours; the
    /// value is the run's length in pages.
    free_runs: BTreeMap<u32, u32>,
}

impl PhysicalAllocator {
    pub(crate) const fn new(page_count: u32) -> Self {
        Self { next_page: 0, page_count, free_runs: BTreeMap::new() }
    }

    pub(crate) fn allocate(&mut self, requested_pages: u32) -> Result<GuestPage, MemoryError> {
        // First fit from the freed runs, lowest address first: reuse keeps
        // the cursor low, and low is where the console's own allocations
        // live.
        if let Some((&start, &length)) =
            self.free_runs.iter().find(|entry| *entry.1 >= requested_pages)
        {
            self.free_runs.remove(&start);
            if length > requested_pages {
                self.free_runs.insert(start + requested_pages, length - requested_pages);
            }
            return Ok(GuestPage(start));
        }
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

    /// Returns a run of pages to the pool, coalescing with any adjacent
    /// freed runs so a large allocation can be satisfied again later.
    ///
    /// Freeing pages the allocator never handed out, or freeing a run
    /// twice, is a caller bug; the pool tolerates it by ignoring overlap
    /// with existing runs rather than corrupting itself.
    pub(crate) fn free(&mut self, start: GuestPage, page_count: u32) {
        if page_count == 0 || start.0 >= self.next_page {
            return;
        }
        let mut begin = start.0;
        let mut end = start.0.saturating_add(page_count).min(self.next_page);
        // Refuse overlap with already-freed runs: trim to the unfreed gap.
        if let Some((&previous, &length)) = self.free_runs.range(..=begin).next_back() {
            let previous_end = previous.saturating_add(length);
            if previous_end >= end {
                return;
            }
            if previous_end > begin {
                begin = previous_end;
            }
            // Adjacent: extend the earlier run instead of inserting.
            if previous_end == begin && self.free_runs.contains_key(&previous) {
                // Also swallow a following run that begins at `end`.
                if let Some(&following) = self.free_runs.get(&end) {
                    self.free_runs.remove(&end);
                    end = end.saturating_add(following);
                }
                self.free_runs.insert(previous, end - previous);
                return;
            }
        }
        if let Some(&following) = self.free_runs.get(&end) {
            self.free_runs.remove(&end);
            end = end.saturating_add(following);
        }
        if end > begin {
            self.free_runs.insert(begin, end - begin);
        }
    }

    /// The next page the bump cursor would hand out.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freed_runs_are_reused_and_coalesced() {
        let mut allocator = PhysicalAllocator::new(64);
        let first = allocator.allocate(4).expect("first");
        let second = allocator.allocate(4).expect("second");
        let third = allocator.allocate(4).expect("third");
        assert_eq!((first.0, second.0, third.0), (0, 4, 8));

        // Free the middle, then the first: they coalesce into one run of 8
        // that satisfies an allocation neither could alone.
        allocator.free(second, 4);
        allocator.free(first, 4);
        let reused = allocator.allocate(8).expect("the coalesced run fits");
        assert_eq!(reused.0, 0, "reuse comes before the cursor");
        assert_eq!(allocator.next_page(), 12, "the cursor did not move");
    }

    #[test]
    fn a_double_free_does_not_corrupt_the_pool() {
        let mut allocator = PhysicalAllocator::new(16);
        let run = allocator.allocate(4).expect("allocates");
        allocator.free(run, 4);
        allocator.free(run, 4);
        assert_eq!(allocator.allocate(4).expect("reused").0, run.0);
        // The second copy of the run must not be handed out again.
        assert_eq!(allocator.allocate(4).expect("fresh").0, 4);
    }

    #[test]
    fn exhaustion_recovers_after_a_free() {
        let mut allocator = PhysicalAllocator::new(8);
        let all = allocator.allocate(8).expect("everything");
        assert!(allocator.allocate(1).is_err(), "nothing left");
        allocator.free(all, 8);
        assert!(allocator.allocate(8).is_ok(), "the pool recovered");
    }
}
