use exbawks_platform::virtual_memory::Placeholder;
use exbawks_types::GuestVa;

use crate::{GUEST_ARENA_SIZE, MemoryError};

/// One reserved 4 GiB host arena for the 32-bit guest address space.
///
/// Host address = arena base + guest virtual address. Every guest page stays
/// inaccessible until a mapping replaces its placeholder range.
#[derive(Debug)]
pub struct GuestArena {
    placeholder: Placeholder,
}

impl GuestArena {
    /// Reserves one 4 GiB-aligned, 4 GiB host placeholder.
    pub fn reserve() -> Result<Self, MemoryError> {
        let arena_len =
            usize::try_from(GUEST_ARENA_SIZE).map_err(|_| MemoryError::HostSizeOverflow)?;
        let placeholder = Placeholder::reserve_aligned(arena_len, arena_len)?;
        let base = placeholder.base() as u64;
        if base & (GUEST_ARENA_SIZE - 1) != 0 {
            return Err(MemoryError::InvalidArenaBase { base });
        }

        tracing::info!(base = format_args!("0x{base:016X}"), "reserved high guest arena");
        Ok(Self { placeholder })
    }

    /// Returns the host arena base.
    #[must_use]
    pub fn base(&self) -> u64 {
        self.placeholder.base() as u64
    }

    /// Returns the host address for one guest virtual address.
    #[must_use]
    pub fn host_address(&self, guest: GuestVa) -> u64 {
        self.base() + u64::from(guest.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn arena_base_is_aligned_and_covers_the_guest_space() {
        let arena = GuestArena::reserve().expect("arena reserve succeeds");
        assert_ne!(arena.base(), 0);
        assert_eq!(arena.base() & (GUEST_ARENA_SIZE - 1), 0);
        assert_eq!(arena.host_address(GuestVa(0)), arena.base());
        assert_eq!(arena.host_address(GuestVa(u32::MAX)), arena.base() + u64::from(u32::MAX));
    }

    #[cfg(windows)]
    #[test]
    fn arena_destruction_releases_the_complete_reservation() {
        let arena = GuestArena::reserve().expect("arena reserve succeeds");
        let base = usize::try_from(arena.base()).expect("base fits the host");
        let len = usize::try_from(GUEST_ARENA_SIZE).expect("length fits the host");
        drop(arena);

        let reclaimed = Placeholder::reserve(Some(base), len)
            .expect("a released arena range can be reserved again");
        assert_eq!(reclaimed.base(), base);
    }

    #[cfg(not(windows))]
    #[test]
    fn arena_reserve_reports_an_unsupported_host() {
        let error = GuestArena::reserve().expect_err("reserve must fail");
        assert!(matches!(error, MemoryError::Platform(_)));
    }
}
