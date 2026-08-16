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
    /// A handle value does not name an open object.
    pub const INVALID_HANDLE: Self = Self(0xC000_0008);

    /// Returns true when the status represents success.
    #[must_use]
    pub const fn is_success(self) -> bool {
        self.0 >> 31 == 0
    }
}
