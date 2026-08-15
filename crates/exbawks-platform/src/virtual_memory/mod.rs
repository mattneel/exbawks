//! Windows placeholder and pagefile-section wrappers.

mod imp;

pub use imp::{MappedView, PagefileSection, Placeholder};

/// Host page protection for a mapped view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageProtection {
    /// The range cannot be accessed.
    NoAccess,
    /// The range can be read.
    ReadOnly,
    /// The range can be read and written.
    ReadWrite,
    /// The range can be read and executed.
    ExecuteRead,
    /// The range can be read, written, and executed.
    ExecuteReadWrite,
}
