use crate::GuestVa;
use bitflags::bitflags;
use serde::{Deserialize, Serialize};

bitflags! {
    /// Guest page permissions.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct MemoryPermissions: u8 {
        /// Read access.
        const READ = 1 << 0;
        /// Write access.
        const WRITE = 1 << 1;
        /// Execute access.
        const EXECUTE = 1 << 2;
    }
}

/// A guest memory access type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccessKind {
    /// A data read.
    Read,
    /// A data write.
    Write,
    /// An instruction fetch.
    Execute,
}

impl AccessKind {
    /// Returns the permission required for this access.
    #[must_use]
    pub const fn required_permission(self) -> MemoryPermissions {
        match self {
            Self::Read => MemoryPermissions::READ,
            Self::Write => MemoryPermissions::WRITE,
            Self::Execute => MemoryPermissions::EXECUTE,
        }
    }
}

/// A dynamic code-generation backend.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackendKind {
    /// Direct guest x86 rewriting through iced-x86.
    #[default]
    DirectRewrite,
    /// Lowering through Cranelift.
    Cranelift,
}

/// The XBE build flavor that selects encoded address keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BuildFlavor {
    /// A retail Xbox image.
    Retail,
    /// A debug Xbox image.
    Debug,
    /// A Sega Chihiro image.
    Chihiro,
    /// An unknown image flavor.
    Unknown,
}

/// A controlled reason that stops guest execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// The guest requested a normal termination.
    GuestExit { code: u32 },
    /// The guest asked the firmware to reboot (`HalReturnToFirmware`). The
    /// routine is the requested `ReturnFirmware*` code; the composition root
    /// decides whether it relaunches the title or ends the run (ADR 0015).
    Reboot { routine: u32 },
    /// The emulator reached an unsupported instruction.
    UnsupportedInstruction { address: GuestVa },
    /// The emulator reached a missing HLE export.
    MissingKernelExport { ordinal: u16 },
    /// The emulator reached a registered but unimplemented HLE export.
    UnimplementedKernelExport { ordinal: u16 },
    /// The guest raised a fault the runtime cannot deliver yet (an invalid
    /// memory access or a divide error; guest exception delivery arrives
    /// with the SEH work).
    GuestFault { address: GuestVa },
    /// Every guest thread is parked and no timer, deadline, or interrupt
    /// can wake one (ADR 0021). The guest has genuinely deadlocked; the
    /// run reports it rather than fabricating a wait's completion.
    GuestDeadlock,
    /// The configured execution budget expired.
    BudgetExhausted,
    /// A person watching the run asked it to stop, by closing its window.
    HostRequested,
    /// The runtime is not implemented yet.
    RuntimeIncomplete,
}
