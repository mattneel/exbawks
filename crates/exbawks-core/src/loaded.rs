use std::sync::Arc;

use exbawks_xbe::XbeImage;

/// One parsed XBE and its retained source bytes.
#[derive(Debug, Clone)]
pub struct LoadedImage {
    image: XbeImage,
    bytes: Arc<[u8]>,
}

impl LoadedImage {
    pub(crate) fn new(image: XbeImage, bytes: Arc<[u8]>) -> Self {
        Self { image, bytes }
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
}
