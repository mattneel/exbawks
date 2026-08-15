#![forbid(unsafe_code)]
#![doc = "Shared value types for Exbawks."]

mod address;
mod execution;

pub use address::{
    AddressError, GuestPa, GuestPage, GuestPageIter, GuestRange, GuestVa, GUEST_PAGE_COUNT,
    GUEST_PAGE_SHIFT, GUEST_PAGE_SIZE,
};
pub use execution::{AccessKind, BackendKind, BuildFlavor, MemoryPermissions, StopReason};
