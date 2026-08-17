#![forbid(unsafe_code)]
#![doc = "Graphics high-level emulation interfaces for Exbawks."]

mod backend;
mod command;
mod error;
mod frontend;
mod nv2a;
mod raster;
mod texture;

pub use backend::{GraphicsBackend, NullGraphicsBackend, NullGraphicsStats};
pub use command::{ClearMask, GraphicsCommand, PrimitiveType, ResourceHandle};
pub use error::GraphicsError;
pub use frontend::GraphicsFrontend;
pub use nv2a::{DmaObject, Nv2aMemory, PushbufferEngine, PusherStats};
pub use raster::{BlendMode, PixelSink, RenderTarget, ScreenVertex, TextureSource, fill_triangle};
pub use texture::{dxt1_texel, dxt3_alpha, dxt5_alpha};
