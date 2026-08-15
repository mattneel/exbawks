/// One structured exit from generated code under the ADR 0006 ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BlockExit {
    /// Execution reached the block end; the guest EIP holds the successor.
    DirectSuccessor = 0,
    /// A conditional branch selected one of two successors.
    ConditionalSuccessor = 1,
    /// An indirect branch stored its target in the guest EIP.
    IndirectSuccessor = 2,
    /// The block reached a kernel HLE call gate.
    KernelCall = 3,
    /// A memory access requires the slow path.
    MemorySlowPath = 4,
    /// The guest EIP holds the first untranslated instruction.
    UnsupportedInstruction = 5,
    /// The configured execution budget expired.
    BudgetExhausted = 6,
}

impl BlockExit {
    /// Decodes one raw exit value from generated code.
    #[must_use]
    pub(crate) const fn from_raw(value: u64) -> Option<Self> {
        if value > 6 {
            return None;
        }

        Some(match value {
            0 => Self::DirectSuccessor,
            1 => Self::ConditionalSuccessor,
            2 => Self::IndirectSuccessor,
            3 => Self::KernelCall,
            4 => Self::MemorySlowPath,
            5 => Self::UnsupportedInstruction,
            _ => Self::BudgetExhausted,
        })
    }

    /// Returns the raw exit code the epilogue loads.
    #[must_use]
    pub(crate) const fn to_raw(self) -> u32 {
        self as u32
    }
}
