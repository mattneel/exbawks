use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The guest page shift for 4 KiB pages.
pub const GUEST_PAGE_SHIFT: u32 = 12;
/// The guest page size in bytes.
pub const GUEST_PAGE_SIZE: u32 = 1 << GUEST_PAGE_SHIFT;
/// The number of pages in the 32-bit guest address space.
pub const GUEST_PAGE_COUNT: usize = 1 << (32 - GUEST_PAGE_SHIFT);

/// A guest virtual address.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct GuestVa(pub u32);

impl GuestVa {
    /// The zero guest address.
    pub const ZERO: Self = Self(0);

    /// Returns the page that contains this address.
    #[must_use]
    pub const fn page(self) -> GuestPage {
        GuestPage(self.0 >> GUEST_PAGE_SHIFT)
    }

    /// Returns the offset inside the containing page.
    #[must_use]
    pub const fn page_offset(self) -> u32 {
        self.0 & (GUEST_PAGE_SIZE - 1)
    }

    /// Adds a byte count and checks for 32-bit overflow.
    #[must_use]
    pub const fn checked_add(self, bytes: u32) -> Option<Self> {
        match self.0.checked_add(bytes) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

impl From<u32> for GuestVa {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<GuestVa> for u32 {
    fn from(value: GuestVa) -> Self {
        value.0
    }
}

impl fmt::Display for GuestVa {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{:08X}", self.0)
    }
}

/// A guest physical address.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct GuestPa(pub u32);

impl GuestPa {
    /// Returns the page that contains this address.
    #[must_use]
    pub const fn page(self) -> GuestPage {
        GuestPage(self.0 >> GUEST_PAGE_SHIFT)
    }

    /// Returns the offset inside the containing page.
    #[must_use]
    pub const fn page_offset(self) -> u32 {
        self.0 & (GUEST_PAGE_SIZE - 1)
    }
}

impl fmt::Display for GuestPa {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{:08X}", self.0)
    }
}

/// A page number in a 32-bit guest address space.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct GuestPage(pub u32);

impl GuestPage {
    /// Returns this page as a table index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Returns the first virtual address in this page.
    #[must_use]
    pub const fn start_va(self) -> GuestVa {
        GuestVa(self.0 << GUEST_PAGE_SHIFT)
    }

    /// Returns the first physical address in this page.
    #[must_use]
    pub const fn start_pa(self) -> GuestPa {
        GuestPa(self.0 << GUEST_PAGE_SHIFT)
    }
}

impl fmt::Display for GuestPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "page 0x{:05X}", self.0)
    }
}

/// A non-empty guest virtual address range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GuestRange {
    start: GuestVa,
    len: u64,
}

impl GuestRange {
    /// Creates a checked guest range.
    pub fn new(start: GuestVa, len: u64) -> Result<Self, AddressError> {
        if len == 0 {
            return Err(AddressError::EmptyRange);
        }

        let end = u64::from(start.0)
            .checked_add(len)
            .ok_or(AddressError::RangeOverflow { start, len })?;

        if end > (u64::from(u32::MAX) + 1) {
            return Err(AddressError::RangeOverflow { start, len });
        }

        Ok(Self { start, len })
    }

    /// Creates a page-aligned range.
    pub fn page_aligned(start: GuestVa, len: u64) -> Result<Self, AddressError> {
        let range = Self::new(start, len)?;
        range.require_page_alignment()?;
        Ok(range)
    }

    /// Returns the first address.
    #[must_use]
    pub const fn start(self) -> GuestVa {
        self.start
    }

    /// Returns the byte length.
    #[must_use]
    pub const fn len(self) -> u64 {
        self.len
    }

    /// Returns the exclusive end as a 64-bit value.
    #[must_use]
    pub const fn end_exclusive(self) -> u64 {
        self.start.0 as u64 + self.len
    }

    /// Returns true when the range is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Returns the number of covered pages.
    #[must_use]
    pub const fn page_count(self) -> u64 {
        let first = self.start.0 as u64 >> GUEST_PAGE_SHIFT;
        let last = (self.end_exclusive() - 1) >> GUEST_PAGE_SHIFT;
        last - first + 1
    }

    /// Returns an iterator over covered pages.
    #[must_use]
    pub const fn pages(self) -> GuestPageIter {
        let first = self.start.0 >> GUEST_PAGE_SHIFT;
        let last_exclusive = ((self.end_exclusive() - 1) >> GUEST_PAGE_SHIFT) as u32 + 1;
        GuestPageIter { next: first, end: last_exclusive }
    }

    /// Verifies page alignment for the start and length.
    pub fn require_page_alignment(self) -> Result<(), AddressError> {
        let mask = u64::from(GUEST_PAGE_SIZE - 1);
        if u64::from(self.start.0) & mask != 0 || self.len & mask != 0 {
            return Err(AddressError::UnalignedRange { start: self.start, len: self.len });
        }

        Ok(())
    }

    /// Returns true when this range contains the address.
    #[must_use]
    pub const fn contains(self, address: GuestVa) -> bool {
        let value = address.0 as u64;
        value >= self.start.0 as u64 && value < self.end_exclusive()
    }
}

/// An iterator over guest pages.
#[derive(Debug, Clone)]
pub struct GuestPageIter {
    next: u32,
    end: u32,
}

impl Iterator for GuestPageIter {
    type Item = GuestPage;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }

        let page = GuestPage(self.next);
        self.next += 1;
        Some(page)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.end - self.next) as usize;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for GuestPageIter {}

/// An address validation error.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AddressError {
    /// A range used a zero length.
    #[error("guest range must not be empty")]
    EmptyRange,
    /// A range exceeded the 32-bit address space.
    #[error("guest range at {start} with length {len} exceeds the 32-bit address space")]
    RangeOverflow { start: GuestVa, len: u64 },
    /// A required page-aligned range was not aligned.
    #[error("guest range at {start} with length {len} is not page aligned")]
    UnalignedRange { start: GuestVa, len: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_rejects_32_bit_overflow() {
        let error = GuestRange::new(GuestVa(0xFFFF_F000), 0x2000).expect_err("range must fail");
        assert!(matches!(error, AddressError::RangeOverflow { .. }));
    }

    #[test]
    fn range_iterates_each_covered_page() {
        let range = GuestRange::new(GuestVa(0x1FFF), 0x2002).expect("range is valid");
        let pages: Vec<_> = range.pages().collect();
        assert_eq!(pages, vec![GuestPage(1), GuestPage(2), GuestPage(3), GuestPage(4)]);
    }

    #[test]
    fn page_alignment_checks_start_and_length() {
        let range = GuestRange::new(GuestVa(0x1000), 0x2000).expect("range is valid");
        assert!(range.require_page_alignment().is_ok());

        let range = GuestRange::new(GuestVa(0x1001), 0x2000).expect("range is valid");
        assert!(range.require_page_alignment().is_err());
    }
}
