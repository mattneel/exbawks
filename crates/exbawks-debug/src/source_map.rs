use exbawks_types::{AccessKind, GuestVa};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One host-code range that one guest instruction produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRange {
    /// The inclusive host code start offset.
    pub host_start: u32,
    /// The exclusive host code end offset.
    pub host_end: u32,
    /// The guest instruction address.
    pub guest_ip: GuestVa,
    /// The guest instruction length.
    pub guest_len: u32,
}

/// One fault record for a faultable host instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultSite {
    /// The host offset of the faultable instruction.
    pub host_offset: u32,
    /// The host instruction length.
    pub host_len: u32,
    /// The guest instruction address.
    pub guest_ip: GuestVa,
    /// The guest memory access type.
    pub access: AccessKind,
    /// The access width in bytes.
    pub width: u8,
    /// The x86 encoding index of the value register.
    pub register: u8,
    /// The host offset where execution resumes after handling.
    pub resume_offset: u32,
    /// The host offset of the generated slow stub.
    pub slow_stub_offset: u32,
}

/// A block metadata construction failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SourceMapError {
    /// A source range was empty.
    #[error("source range for guest {guest_ip} is empty")]
    EmptyRange {
        /// The guest instruction address.
        guest_ip: GuestVa,
    },
    /// Two source ranges overlapped.
    #[error("source range for guest {guest_ip} overlaps its predecessor")]
    OverlappingRanges {
        /// The guest instruction address of the second range.
        guest_ip: GuestVa,
    },
    /// Two fault sites shared one host offset.
    #[error("duplicate fault site at host offset {host_offset}")]
    DuplicateFaultSite {
        /// The duplicated host offset.
        host_offset: u32,
    },
}

/// Immutable sorted source and fault metadata for one sealed block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSourceMap {
    ranges: Box<[SourceRange]>,
    faults: Box<[FaultSite]>,
}

impl BlockSourceMap {
    /// Sorts, validates, and freezes block metadata.
    pub fn new(
        mut ranges: Vec<SourceRange>,
        mut faults: Vec<FaultSite>,
    ) -> Result<Self, SourceMapError> {
        ranges.sort_unstable_by_key(|range| range.host_start);
        for (index, range) in ranges.iter().enumerate() {
            if range.host_start >= range.host_end {
                return Err(SourceMapError::EmptyRange { guest_ip: range.guest_ip });
            }
            if index > 0 && ranges[index - 1].host_end > range.host_start {
                return Err(SourceMapError::OverlappingRanges { guest_ip: range.guest_ip });
            }
        }

        faults.sort_unstable_by_key(|fault| fault.host_offset);
        for pair in faults.windows(2) {
            if pair[0].host_offset == pair[1].host_offset {
                return Err(SourceMapError::DuplicateFaultSite {
                    host_offset: pair[1].host_offset,
                });
            }
        }

        Ok(Self { ranges: ranges.into_boxed_slice(), faults: faults.into_boxed_slice() })
    }

    /// Returns every source range in host order.
    #[must_use]
    pub fn ranges(&self) -> &[SourceRange] {
        &self.ranges
    }

    /// Returns every fault site in host order.
    #[must_use]
    pub fn faults(&self) -> &[FaultSite] {
        &self.faults
    }

    /// Returns the source range that contains one host offset.
    #[must_use]
    pub fn source_for_host_offset(&self, offset: u32) -> Option<&SourceRange> {
        let candidate = self.ranges.partition_point(|range| range.host_start <= offset);
        let range = self.ranges.get(candidate.checked_sub(1)?)?;
        (offset < range.host_end).then_some(range)
    }

    /// Returns the fault site that starts at one host offset.
    #[must_use]
    pub fn fault_for_host_offset(&self, offset: u32) -> Option<&FaultSite> {
        let index = self.faults.binary_search_by_key(&offset, |fault| fault.host_offset).ok()?;
        self.faults.get(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(host_start: u32, host_end: u32, guest_ip: u32) -> SourceRange {
        SourceRange { host_start, host_end, guest_ip: GuestVa(guest_ip), guest_len: 2 }
    }

    fn fault(host_offset: u32) -> FaultSite {
        FaultSite {
            host_offset,
            host_len: 3,
            guest_ip: GuestVa(0x1000),
            access: AccessKind::Read,
            width: 4,
            register: 0,
            resume_offset: host_offset + 3,
            slow_stub_offset: 0x80,
        }
    }

    #[test]
    fn lookup_finds_ranges_at_both_boundaries() {
        let map = BlockSourceMap::new(
            vec![range(10, 20, 0x1002), range(0, 10, 0x1000), range(20, 23, 0x1004)],
            Vec::new(),
        )
        .expect("map is valid");

        assert_eq!(map.source_for_host_offset(0).map(|r| r.guest_ip), Some(GuestVa(0x1000)));
        assert_eq!(map.source_for_host_offset(9).map(|r| r.guest_ip), Some(GuestVa(0x1000)));
        assert_eq!(map.source_for_host_offset(10).map(|r| r.guest_ip), Some(GuestVa(0x1002)));
        assert_eq!(map.source_for_host_offset(22).map(|r| r.guest_ip), Some(GuestVa(0x1004)));
    }

    #[test]
    fn unknown_host_locations_return_no_match() {
        let map =
            BlockSourceMap::new(vec![range(4, 8, 0x1000), range(12, 16, 0x1002)], vec![fault(4)])
                .expect("map is valid");

        assert!(map.source_for_host_offset(3).is_none());
        assert!(map.source_for_host_offset(8).is_none());
        assert!(map.source_for_host_offset(11).is_none());
        assert!(map.source_for_host_offset(16).is_none());
        assert!(map.source_for_host_offset(u32::MAX).is_none());
        assert!(map.fault_for_host_offset(3).is_none());
        assert!(map.fault_for_host_offset(5).is_none());
    }

    #[test]
    fn fault_lookup_matches_exact_offsets() {
        let map = BlockSourceMap::new(Vec::new(), vec![fault(9), fault(2), fault(30)])
            .expect("map is valid");

        assert_eq!(map.fault_for_host_offset(2).map(|f| f.host_offset), Some(2));
        assert_eq!(map.fault_for_host_offset(9).map(|f| f.host_offset), Some(9));
        assert_eq!(map.fault_for_host_offset(30).map(|f| f.host_offset), Some(30));
        assert!(map.fault_for_host_offset(10).is_none());
    }

    #[test]
    fn invalid_metadata_is_rejected() {
        let empty = BlockSourceMap::new(vec![range(4, 4, 0x1000)], Vec::new());
        assert!(matches!(empty, Err(SourceMapError::EmptyRange { .. })));

        let overlap =
            BlockSourceMap::new(vec![range(0, 6, 0x1000), range(5, 9, 0x1002)], Vec::new());
        assert!(matches!(overlap, Err(SourceMapError::OverlappingRanges { .. })));

        let duplicate = BlockSourceMap::new(Vec::new(), vec![fault(7), fault(7)]);
        assert!(matches!(duplicate, Err(SourceMapError::DuplicateFaultSite { .. })));
    }
}
