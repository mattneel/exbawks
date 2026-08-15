use exbawks_platform::PlatformError;
use exbawks_types::{BackendKind, GuestVa};
use thiserror::Error;

/// A dynamic recompilation failure.
#[derive(Debug, Error)]
pub enum JitError {
    /// A selected backend is not available in this build.
    #[error("codegen backend {backend:?} is not available: {reason}")]
    BackendUnavailable {
        /// The unavailable backend.
        backend: BackendKind,
        /// The reason the backend is unavailable.
        reason: &'static str,
    },
    /// A translation request contained no instructions.
    #[error("cannot compile an empty guest block")]
    EmptyBlock,
    /// A decoded block ends outside the 32-bit guest address space.
    #[error("block at {start} ends outside the guest address space")]
    BlockEndOverflow {
        /// The guest block start.
        start: GuestVa,
    },
    /// Generated code returned an unknown exit value.
    #[error("generated code returned malformed exit value 0x{value:016X}")]
    MalformedExit {
        /// The raw returned value.
        value: u64,
    },
    /// A host platform operation failed.
    #[error(transparent)]
    Platform(#[from] PlatformError),
}
