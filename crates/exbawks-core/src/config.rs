use exbawks_types::{BackendKind, GUEST_PAGE_SIZE};
use serde::{Deserialize, Serialize};

/// Stable configuration for one emulator instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmulatorConfig {
    /// The emulated physical RAM byte count.
    pub physical_memory_bytes: usize,
    /// The selected dynamic code-generation backend.
    pub backend: BackendKind,
    /// The maximum decoded instructions in one block.
    pub max_block_instructions: usize,
    /// The maximum decoded bytes in one block.
    pub max_block_bytes: usize,
    /// The maximum kernel thunk count for inspection.
    pub max_kernel_thunks: usize,
}

impl EmulatorConfig {
    /// The original retail Xbox RAM size.
    pub const RETAIL_RAM_BYTES: usize = 64 * 1024 * 1024;
    /// A common development-kit RAM size.
    pub const DEVELOPMENT_RAM_BYTES: usize = 128 * 1024 * 1024;
    /// The largest supported RAM size: the development-kit ceiling. Keeping
    /// the cached physical window (`0x8000_0000 + ram`, ADR 0010) far below
    /// the device MMIO space (`0xFD00_0000`) is a hard requirement.
    pub const MAX_RAM_BYTES: usize = Self::DEVELOPMENT_RAM_BYTES;

    /// Returns true when physical memory uses complete guest pages and fits
    /// under the console ceiling.
    #[must_use]
    pub fn physical_memory_is_aligned(&self) -> bool {
        let page_size = usize::try_from(GUEST_PAGE_SIZE).unwrap_or(4096);
        self.physical_memory_bytes != 0
            && self.physical_memory_bytes.is_multiple_of(page_size)
            && self.physical_memory_bytes <= Self::MAX_RAM_BYTES
    }
}

impl Default for EmulatorConfig {
    fn default() -> Self {
        Self {
            physical_memory_bytes: Self::RETAIL_RAM_BYTES,
            backend: BackendKind::DirectRewrite,
            max_block_instructions: 256,
            max_block_bytes: 4096,
            max_kernel_thunks: 4096,
        }
    }
}
