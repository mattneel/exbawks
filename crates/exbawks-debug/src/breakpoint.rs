use std::collections::BTreeSet;

use exbawks_types::GuestVa;

/// A deterministic set of guest instruction breakpoints.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BreakpointSet {
    addresses: BTreeSet<GuestVa>,
}

impl BreakpointSet {
    /// Creates an empty breakpoint set.
    #[must_use]
    pub const fn new() -> Self {
        Self { addresses: BTreeSet::new() }
    }

    /// Adds a guest breakpoint.
    pub fn insert(&mut self, address: GuestVa) -> bool {
        self.addresses.insert(address)
    }

    /// Removes a guest breakpoint.
    pub fn remove(&mut self, address: GuestVa) -> bool {
        self.addresses.remove(&address)
    }

    /// Returns true when one address is a breakpoint.
    #[must_use]
    pub fn contains(&self, address: GuestVa) -> bool {
        self.addresses.contains(&address)
    }

    /// Returns the number of breakpoints.
    #[must_use]
    pub fn len(&self) -> usize {
        self.addresses.len()
    }

    /// Returns true when no breakpoints exist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.addresses.is_empty()
    }

    /// Returns breakpoints in address order.
    pub fn iter(&self) -> impl Iterator<Item = GuestVa> + '_ {
        self.addresses.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoints_remain_sorted() {
        let mut set = BreakpointSet::new();
        assert!(set.insert(GuestVa(0x2000)));
        assert!(set.insert(GuestVa(0x1000)));

        assert_eq!(set.iter().collect::<Vec<_>>(), vec![GuestVa(0x1000), GuestVa(0x2000)]);
    }
}
