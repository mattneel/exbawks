use crate::PlatformError;

use super::super::{CoalesceError, PageProtection, ReplaceError, RestoreError};

/// A pagefile-backed physical memory section.
#[derive(Debug, Clone)]
pub struct PagefileSection {
    len: usize,
}

impl PagefileSection {
    /// Creates a pagefile-backed section.
    pub fn new(len: usize) -> Result<Self, PlatformError> {
        let _ = len;
        Err(PlatformError::Unsupported("pagefile sections require Windows"))
    }

    /// Returns the section length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns true when the section has zero length.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Replaces a placeholder with a section view.
    ///
    /// A failure returns the preserved placeholder to the caller.
    pub fn map_replace(
        &self,
        placeholder: Placeholder,
        offset: u64,
        protection: PageProtection,
    ) -> Result<MappedView, ReplaceError> {
        let _ = (offset, protection);
        Err(ReplaceError {
            placeholder,
            error: PlatformError::Unsupported("section views require Windows"),
        })
    }
}

/// A reserved Windows placeholder.
#[derive(Debug)]
pub struct Placeholder {
    base: usize,
    len: usize,
}

impl Placeholder {
    /// Reserves a placeholder.
    pub fn reserve(base: Option<usize>, len: usize) -> Result<Self, PlatformError> {
        let _ = (base, len);
        Err(PlatformError::Unsupported("placeholders require Windows"))
    }

    /// Reserves one aligned placeholder at a host-selected address.
    pub fn reserve_aligned(alignment: usize, len: usize) -> Result<Self, PlatformError> {
        let _ = (alignment, len);
        Err(PlatformError::Unsupported("placeholders require Windows"))
    }

    /// Returns the placeholder base.
    #[must_use]
    pub const fn base(&self) -> usize {
        self.base
    }

    /// Returns the placeholder length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns true when the placeholder has zero length.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Splits this placeholder and returns the owned tail range.
    pub fn split_off(&mut self, offset: usize) -> Result<Self, PlatformError> {
        let _ = offset;
        Err(PlatformError::Unsupported("placeholders require Windows"))
    }

    /// Coalesces this placeholder with the adjacent following placeholder.
    pub fn coalesce_with(&mut self, next: Self) -> Result<(), CoalesceError> {
        Err(CoalesceError {
            next,
            error: PlatformError::Unsupported("placeholders require Windows"),
        })
    }
}

/// A mapped section view.
#[derive(Debug)]
pub struct MappedView {
    base: usize,
    len: usize,
}

impl MappedView {
    /// Returns the host base address.
    #[must_use]
    pub const fn base(&self) -> usize {
        self.base
    }

    /// Returns the mapped length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns true when the view has zero length.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the current host protection.
    #[must_use]
    pub const fn protection(&self) -> PageProtection {
        PageProtection::NoAccess
    }

    /// Copies bytes out of the mapped view.
    pub fn read_at(&self, offset: usize, output: &mut [u8]) -> Result<(), PlatformError> {
        let _ = (offset, output);
        Err(PlatformError::Unsupported("section views require Windows"))
    }

    /// Copies bytes into the mapped view.
    pub fn write_at(&self, offset: usize, input: &[u8]) -> Result<(), PlatformError> {
        let _ = (offset, input);
        Err(PlatformError::Unsupported("section views require Windows"))
    }

    /// Changes the host protection of the complete view.
    pub fn protect(&mut self, protection: PageProtection) -> Result<(), PlatformError> {
        let _ = protection;
        Err(PlatformError::Unsupported("section views require Windows"))
    }

    /// Unmaps the view and returns the restored placeholder.
    ///
    /// A failure returns the still-mapped view to the caller.
    pub fn unmap_restore(self) -> Result<Placeholder, RestoreError> {
        Err(RestoreError {
            view: self,
            error: PlatformError::Unsupported("section views require Windows"),
        })
    }
}
