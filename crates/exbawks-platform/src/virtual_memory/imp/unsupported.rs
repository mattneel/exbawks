use crate::PlatformError;

use super::super::PageProtection;

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
    pub fn map_replace(
        &self,
        placeholder: Placeholder,
        offset: u64,
        protection: PageProtection,
    ) -> Result<MappedView, PlatformError> {
        let _ = (self, placeholder, offset, protection);
        Err(PlatformError::Unsupported("section views require Windows"))
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
}
