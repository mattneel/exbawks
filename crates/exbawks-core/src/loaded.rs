use std::sync::Arc;

use exbawks_xbe::XbeImage;

use crate::KernelThunkTable;

/// One parsed XBE and its retained source bytes.
#[derive(Debug, Clone)]
pub struct LoadedImage {
    image: XbeImage,
    bytes: Arc<[u8]>,
    kernel_thunks: KernelThunkTable,
}

impl LoadedImage {
    pub(crate) fn new(image: XbeImage, bytes: Arc<[u8]>, kernel_thunks: KernelThunkTable) -> Self {
        Self { image, bytes, kernel_thunks }
    }

    /// Returns the parsed XBE model.
    #[must_use]
    pub const fn image(&self) -> &XbeImage {
        &self.image
    }

    /// Returns the original XBE bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the kernel import table as parsed before gate patching.
    #[must_use]
    pub const fn kernel_thunks(&self) -> &KernelThunkTable {
        &self.kernel_thunks
    }
}
