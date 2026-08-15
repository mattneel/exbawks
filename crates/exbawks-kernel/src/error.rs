use thiserror::Error;

/// A kernel HLE registration or dispatch failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum KernelError {
    /// An ordinal already has an export.
    #[error("kernel export ordinal {ordinal} is already registered")]
    DuplicateOrdinal { ordinal: u16 },
    /// No export exists for an ordinal.
    #[error("kernel export ordinal {ordinal} is not registered")]
    MissingOrdinal { ordinal: u16 },
}
