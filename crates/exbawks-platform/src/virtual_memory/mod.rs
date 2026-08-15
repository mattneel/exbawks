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

/// A failed view replacement that returns the preserved placeholder.
#[derive(Debug, Error)]
#[error("section view replacement failed: {error}")]
pub struct ReplaceError {
    /// The placeholder that remains owned by the caller.
    pub placeholder: Placeholder,
    /// The failure reason.
    #[source]
    pub error: PlatformError,
}

/// A failed view unmap that returns the still-mapped view.
#[derive(Debug, Error)]
#[error("mapped view restore failed: {error}")]
pub struct RestoreError {
    /// The view that remains mapped and owned by the caller.
    pub view: MappedView,
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
