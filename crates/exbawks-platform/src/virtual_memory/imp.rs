#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{MappedView, PagefileSection, Placeholder};

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
pub use unsupported::{MappedView, PagefileSection, Placeholder};
