use exbawks_platform::PlatformError;
use exbawks_types::{AccessKind, AddressError, GuestPa, GuestVa};
use thiserror::Error;

/// A guest memory failure.
#[derive(Debug, Error)]
pub enum MemoryError {
    /// A guest range was invalid.
    #[error(transparent)]
    Address(#[from] AddressError),
    /// The physical allocator cannot satisfy a request.
    #[error("guest physical memory is exhausted for {requested_pages} pages")]
    OutOfPhysicalMemory { requested_pages: u32 },
    /// A guest virtual address is not mapped.
    #[error("guest address {address} is not mapped for {access:?}")]
    Unmapped { address: GuestVa, access: AccessKind },
    /// A guest page does not permit an access.
    #[error("guest address {address} does not permit {access:?}")]
    AccessDenied { address: GuestVa, access: AccessKind },
    /// A guest address refers to MMIO.
    #[error("guest address {address} refers to MMIO handler {handler_id}")]
    Mmio { address: GuestVa, handler_id: u16 },
    /// A physical page lies outside the configured RAM section.
    #[error("guest physical address {address} exceeds configured RAM")]
    PhysicalOutOfRange { address: GuestPa },
    /// A mapping operation overlapped an existing mapping.
    #[error("guest virtual address {address} is already mapped")]
    AlreadyMapped { address: GuestVa },
    /// A byte count cannot fit in the current host process.
    #[error("guest byte count does not fit in host usize")]
    HostSizeOverflow,
    /// A high arena base is not 4 GiB aligned or cannot contain the guest space.
    #[error("host arena base 0x{base:016X} cannot contain one aligned 4 GiB guest arena")]
    InvalidArenaBase { base: u64 },
    /// A physical RAM section size is zero, unaligned, or larger than 4 GiB.
    #[error("physical RAM section size {bytes} is invalid")]
    InvalidPhysicalSize { bytes: u64 },
    /// A section view offset is not guest-page aligned.
    #[error("guest physical address {address} is not page aligned")]
    UnalignedPhysicalAddress { address: GuestPa },
    /// Two planned views overlap in the guest arena.
    #[error("guest arena view at {address} overlaps an existing view")]
    ArenaViewOverlap { address: GuestVa },
    /// A host platform operation failed.
    #[error(transparent)]
    Platform(#[from] PlatformError),
}
