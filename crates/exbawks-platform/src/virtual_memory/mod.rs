//! Windows placeholder and pagefile-section wrappers.
//!
//! Placeholders and mapped views own their host ranges exclusively.
//!
//! ```compile_fail
//! fn requires_clone<T: Clone>() {}
//! requires_clone::<exbawks_platform::virtual_memory::Placeholder>();
//! ```
//!
//! ```compile_fail
//! fn requires_clone<T: Clone>() {}
//! requires_clone::<exbawks_platform::virtual_memory::MappedView>();
//! ```

mod imp;

use thiserror::Error;

use crate::PlatformError;

pub use imp::{MappedView, PagefileSection, Placeholder};

/// A failed coalesce that returns the unconsumed adjacent placeholder.
#[derive(Debug, Error)]
#[error("placeholder coalesce failed: {error}")]
pub struct CoalesceError {
    /// The right-hand placeholder that remains owned by the caller.
    pub next: Placeholder,
    /// The failure reason.
    #[source]
    pub error: PlatformError,
}

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
