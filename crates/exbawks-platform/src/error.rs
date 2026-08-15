use thiserror::Error;

/// A host platform failure.
#[derive(Debug, Error)]
pub enum PlatformError {
    /// The current host does not support an operation.
    #[error("host operation is unsupported: {0}")]
    Unsupported(&'static str),
    /// A caller supplied an invalid range or offset.
    #[error("invalid host memory argument: {0}")]
    InvalidArgument(&'static str),
    /// A Windows API returned a failure code.
    #[error("Windows operation {operation} failed with error {code}")]
    Win32 {
        /// The failed operation.
        operation: &'static str,
        /// The value from `GetLastError`.
        code: u32,
    },
}
