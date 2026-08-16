/// A 32-bit Xbox kernel status value.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct KernelStatus(pub u32);

impl KernelStatus {
    /// Successful completion.
    pub const SUCCESS: Self = Self(0x0000_0000);
    /// The requested operation is not implemented.
    pub const NOT_IMPLEMENTED: Self = Self(0xC000_0002);
    /// One parameter is invalid.
    pub const INVALID_PARAMETER: Self = Self(0xC000_000D);
    /// One guest address is invalid.
    pub const ACCESS_VIOLATION: Self = Self(0xC000_0005);
    /// Insufficient guest resources exist to complete the request.
    pub const INSUFFICIENT_RESOURCES: Self = Self(0xC000_009A);
    /// Not enough virtual memory or paging files exist for the request.
    pub const NO_MEMORY: Self = Self(0xC000_0017);
    /// A handle value does not name an open object.
    pub const INVALID_HANDLE: Self = Self(0xC000_0008);
    /// The caller's buffer is too small for the returned data.
    pub const BUFFER_TOO_SMALL: Self = Self(0xC000_0023);
    /// The requested access to the object was denied.
    pub const ACCESS_DENIED: Self = Self(0xC000_0022);
    /// The named file or object was not found.
    pub const OBJECT_NAME_NOT_FOUND: Self = Self(0xC000_0034);
    /// The end of the file was reached.
    pub const END_OF_FILE: Self = Self(0xC000_0011);

    /// Returns true when the status represents success.
    #[must_use]
    pub const fn is_success(self) -> bool {
        self.0 >> 31 == 0
    }
}
