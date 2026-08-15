#![forbid(unsafe_code)]
#![doc = "Shared value types for Exbawks."]

mod address;
mod execution;

pub use address::{
    AddressError, GUEST_PAGE_COUNT, GUEST_PAGE_SHIFT, GUEST_PAGE_SIZE, GuestPa, GuestPage,
    GuestPageIter, GuestRange, GuestVa,
};
pub use execution::{AccessKind, BackendKind, BuildFlavor, MemoryPermissions, StopReason};
