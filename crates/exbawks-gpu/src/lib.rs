#![forbid(unsafe_code)]
#![doc = "Graphics high-level emulation interfaces for Exbawks."]

mod backend;
mod combiner;
mod command;
mod error;
mod frontend;
mod nv2a;
mod raster;
mod shader;
mod texture;

pub use backend::{GraphicsBackend, NullGraphicsBackend, NullGraphicsStats};
pub use combiner::{
    CombinerState, Registers as CombinerRegisters, STAGES as COMBINER_STAGES,
    Stage as CombinerStage, evaluate as evaluate_combiner,
};
pub use command::{ClearMask, GraphicsCommand, PrimitiveType, ResourceHandle};
pub use error::GraphicsError;
pub use frontend::GraphicsFrontend;
pub use nv2a::{DmaObject, Nv2aMemory, PushbufferEngine, PusherStats};
pub use raster::{
    AlphaTest, BlendState, CullState, DepthState, PipelineState, PixelSink, RenderTarget,
    ScreenVertex, TextureSource, fill_triangle,
};
pub use shader::{INPUT_REGISTERS, ShaderResult, execute};
pub use texture::{dxt_opaque_texel, dxt1_texel, dxt3_alpha, dxt5_alpha};
