#![doc = "Guest memory mapping and access services for Exbawks."]

mod allocator;
mod arena;
mod error;
mod page_table;
mod software;
mod windows_plan;
mod windows_space;

pub use arena::GuestArena;
pub use error::MemoryError;
pub use page_table::{PageDescriptor, PageKind, PageTable, WatchFlags};
pub use software::{GuestMemory, SoftwareAddressSpace};
pub use windows_plan::{GUEST_ARENA_SIZE, GuestViewPlan, WindowsArenaPlan};
pub use windows_space::WindowsAddressSpace;
