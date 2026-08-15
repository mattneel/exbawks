#![forbid(unsafe_code)]
#![doc = "Checked XBE parsing for Exbawks."]

mod error;
mod format;
mod image;
mod reader;

pub use error::XbeError;
pub use format::{XbeHeader, XbeSection, XbeSectionFlags};
pub use image::XbeImage;
