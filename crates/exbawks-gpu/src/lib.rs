#![forbid(unsafe_code)]
#![doc = "Graphics high-level emulation interfaces for Exbawks."]

mod backend;
mod command;
mod error;
mod frontend;

pub use backend::{GraphicsBackend, NullGraphicsBackend, NullGraphicsStats};
pub use command::{ClearMask, GraphicsCommand, PrimitiveType, ResourceHandle};
pub use error::GraphicsError;
pub use frontend::GraphicsFrontend;
