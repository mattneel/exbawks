#![forbid(unsafe_code)]
#![doc = "Graphics high-level emulation interfaces for Exbawks."]

mod backend;
mod command;
mod error;
mod frontend;
mod nv2a;
mod raster;

pub use backend::{GraphicsBackend, NullGraphicsBackend, NullGraphicsStats};
pub use command::{ClearMask, GraphicsCommand, PrimitiveType, ResourceHandle};
pub use error::GraphicsError;
pub use frontend::GraphicsFrontend;
pub use nv2a::{DmaObject, Nv2aMemory, PushbufferEngine, PusherStats};
pub use raster::{PixelSink, RenderTarget, ScreenVertex, fill_triangle};
