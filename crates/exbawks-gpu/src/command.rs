use serde::{Deserialize, Serialize};

/// An opaque guest graphics resource handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct ResourceHandle(pub u32);

/// A primitive topology for a draw command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PrimitiveType {
    /// Independent points.
    PointList,
    /// Independent line segments.
    LineList,
    /// Connected line segments.
    LineStrip,
    /// Independent triangles.
    TriangleList,
    /// Connected triangles.
    TriangleStrip,
    /// Triangle fan topology.
    TriangleFan,
}

/// Buffers selected by a clear command.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClearMask {
    /// Clear the color target.
    pub color: bool,
    /// Clear the depth target.
    pub depth: bool,
    /// Clear the stencil target.
    pub stencil: bool,
}

/// A host-neutral command emitted by graphics HLE.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GraphicsCommand {
    /// Creates the first logical graphics device.
    CreateDevice {
        /// The requested back-buffer width.
        width: u32,
        /// The requested back-buffer height.
        height: u32,
    },
    /// Clears selected render targets.
    Clear {
        /// The selected targets.
        mask: ClearMask,
        /// RGBA color components.
        color: [f32; 4],
        /// The depth clear value.
        depth: f32,
        /// The stencil clear value.
        stencil: u8,
    },
    /// Draws non-indexed guest primitives.
    Draw {
        /// The guest primitive topology.
        primitive: PrimitiveType,
        /// The first guest vertex.
        first_vertex: u32,
        /// The guest vertex count.
        vertex_count: u32,
    },
    /// Presents the active back buffer.
    Present,
    /// Records an unknown graphics method for later analysis.
    UnknownMethod {
        /// The guest method identifier.
        method: u32,
        /// The method data word.
        data: u32,
    },
}
