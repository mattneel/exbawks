use exbawks_types::BackendKind;
use thiserror::Error;

/// A dynamic recompilation failure.
#[derive(Debug, Error)]
pub enum JitError {
    /// A selected backend is not available in this build.
    #[error("codegen backend {backend:?} is not available: {reason}")]
    BackendUnavailable { backend: BackendKind, reason: &'static str },
    /// A translation request contained no instructions.
    #[error("cannot compile an empty guest block")]
    EmptyBlock,
}
