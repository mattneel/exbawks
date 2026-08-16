use crate::{KernelCallContext, KernelStatus};

/// One callable kernel HLE export.
pub trait KernelExport: Send + Sync {
    /// Returns the Xbox kernel export ordinal.
    fn ordinal(&self) -> u16;

    /// Returns a diagnostic export name.
    fn name(&self) -> &'static str;

    /// Returns the stdcall argument bytes the runtime pops after the call.
    fn stack_bytes(&self) -> u16 {
        0
    }

    /// Returns true when the export is a named placeholder without semantics.
    fn is_stub(&self) -> bool {
        false
    }

    /// Executes the export against checked guest state.
    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus;
}

/// A named export that reports an unimplemented status.
#[derive(Debug, Clone, Copy)]
pub struct StubExport {
    ordinal: u16,
    name: &'static str,
}

impl StubExport {
    /// Creates a stub for one ordinal.
    #[must_use]
    pub const fn new(ordinal: u16, name: &'static str) -> Self {
        Self { ordinal, name }
    }
}

impl KernelExport for StubExport {
    fn ordinal(&self) -> u16 {
        self.ordinal
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn is_stub(&self) -> bool {
        true
    }

    fn call(&self, _context: &mut KernelCallContext<'_>) -> KernelStatus {
        KernelStatus::NOT_IMPLEMENTED
    }
}
