#![doc = "Guest memory mapping and access services for Exbawks."]

mod error;
mod page_table;
mod software;
mod windows_plan;

pub use error::MemoryError;
pub use page_table::{PageDescriptor, PageKind, PageTable, WatchFlags};
pub use software::{GuestMemory, SoftwareAddressSpace};
pub use windows_plan::{GuestViewPlan, WindowsArenaPlan, GUEST_ARENA_SIZE};
