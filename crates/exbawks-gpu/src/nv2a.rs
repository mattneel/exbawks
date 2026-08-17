//! The NV2A pushbuffer engine (GPU-M0).
//!
//! Direct3D drives the GPU by writing command dwords into a pushbuffer in
//! guest RAM and advancing the channel's `DMA_PUT` register; the hardware's
//! DMA pusher walks `DMA_GET` toward `DMA_PUT`, decoding method headers and
//! control words. This engine replays that walk when the emulator observes
//! a `DMA_PUT` write: control flow (jumps, calls, returns) follows the
//! hardware rules, methods are counted and traced, and the small method set
//! with externally visible side effects is implemented — object binding
//! through the `RAMHT` hash table and the back-end semaphore release that
//! Direct3D's fences read from memory.
//!
//! Everything here is pure logic over a [`Nv2aMemory`] view; the emulator
//! adapts it to guest RAM through the cached physical window.

use std::collections::HashMap;

/// Physical-memory access for the pusher.
///
/// Addresses are guest physical; the emulator reaches them through the
/// cached window. Reads and writes are best-effort: an out-of-range access
/// returns `false` and the pusher abandons the submission.
pub trait Nv2aMemory {
    /// Reads one little-endian dword at a physical address.
    fn read_dword(&self, physical: u32) -> Option<u32>;

    /// Writes one little-endian dword at a physical address.
    fn write_dword(&self, physical: u32, value: u32) -> bool;

    /// Fills `count` consecutive dwords with one value, returning how many
    /// were written.
    ///
    /// A clear covers a whole surface, so the per-dword path costs far more
    /// than the work itself; an implementation backed by real memory should
    /// override this with a bulk write.
    fn fill_dwords(&self, physical: u32, value: u32, count: u32) -> u32 {
        let mut written = 0;
        for index in 0..count {
            if !self.write_dword(physical.wrapping_add(index * 4), value) {
                break;
            }
            written += 1;
        }
        written
    }
}

/// One resolved DMA object from instance memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaObject {
    /// The object class (low 12 bits of the first dword).
    pub class: u32,
    /// The byte limit of the target range.
    pub limit: u32,
    /// The target base physical address (frame plus adjust).
    pub base: u32,
}

/// The render-target state a channel programs before drawing.
///
/// The color surface is the only one modeled: the title's frames land in
/// it, and it is what a capture reads back.
#[derive(Debug, Default, Clone, Copy)]
struct SurfaceState {
    /// The color surface's context-DMA object, once bound.
    color_dma: Option<DmaObject>,
    /// The color surface's byte offset inside that object.
    color_offset: u32,
    /// The depth surface's byte offset inside the same object.
    zeta_offset: u32,
    /// The distance between depth scanlines in bytes.
    zeta_pitch: u32,
    /// The distance between color scanlines in bytes.
    color_pitch: u32,
    /// The surface clip's left edge and width in pixels.
    clip_x: u32,
    clip_width: u32,
    /// The surface clip's top edge and height in pixels.
    clip_y: u32,
    clip_height: u32,
    /// The ARGB value a color clear writes.
    clear_color: u32,
    /// The packed depth and stencil value a depth clear writes.
    clear_zstencil: u32,
    /// The clear rectangle's horizontal bounds (left, right).
    clear_x: (u32, u32),
    /// The clear rectangle's vertical bounds (top, bottom).
    clear_y: (u32, u32),
}

/// The first texture unit's programmed state.
///
/// Only unit zero is modeled: a title's background and interface art come
/// through it, and the later units feed combiner stages this engine does
/// not run.
#[derive(Debug, Default, Clone, Copy)]
struct TextureState {
    /// The texture's byte offset inside its context DMA.
    offset: u32,
    /// The raw format word: color code, dimensions, and mip levels.
    format: u32,
    /// The distance between texture rows in bytes, for linear formats.
    pitch: u32,
    /// The width and height a linear format carries separately.
    rect: (u32, u32),
    /// Whether the unit is enabled by its control word.
    enabled: bool,
}

impl SurfaceState {
    /// The rectangle a clear covers: the clear rectangle intersected with
    /// the surface clip, and with what the hardware can address at `pitch`.
    fn clear_bounds(&self, pitch: u32) -> Option<(u32, u32, u32, u32)> {
        if pitch == 0 {
            return None;
        }
        let addressable = (pitch / 4).min(MAX_SURFACE_DIMENSION);
        let left = self.clear_x.0.max(self.clip_x);
        let right =
            self.clear_x.1.min(self.clip_x.saturating_add(self.clip_width)).min(addressable);
        let top = self.clear_y.0.max(self.clip_y);
        let bottom = self
            .clear_y
            .1
            .min(self.clip_y.saturating_add(self.clip_height))
            .min(MAX_SURFACE_DIMENSION);
        (left < right && top < bottom).then_some((left, right, top, bottom))
    }
}

impl TextureState {
    /// The color format code the hardware samples with.
    fn color_format(self) -> u32 {
        (self.format >> 8) & 0xFF
    }

    /// The texture's extent in texels.
    ///
    /// Swizzled and compressed formats carry base-two logarithms in the
    /// format word, and those are authoritative: a linear texture cannot
    /// express its size that way and leaves them zero, carrying an explicit
    /// rectangle instead — which stays behind in the register when the next
    /// texture is a compressed one, so reading it first mis-sizes the image.
    fn extent(self) -> (u32, u32) {
        // A linear format cannot express its size as logarithms and carries
        // an explicit rectangle; every other format's logarithms are
        // authoritative, including the one-by-one case where both read
        // zero and the rectangle left behind by an earlier texture would
        // otherwise be believed.
        if self.is_linear() {
            return self.rect;
        }
        let width_log2 = (self.format >> 20) & 0xF;
        let height_log2 = (self.format >> 24) & 0xF;
        (1 << width_log2, 1 << height_log2)
    }

    /// Whether the format stores its texels row by row at a pitch.
    fn is_linear(self) -> bool {
        matches!(
            self.color_format(),
            TEXTURE_FORMAT_LINEAR_A8R8G8B8 | TEXTURE_FORMAT_LINEAR_X8R8G8B8
        )
    }
}

/// One vertex attribute's declared layout.
#[derive(Debug, Default, Clone, Copy)]
struct AttributeFormat {
    /// The component type code (`2` is 32-bit float).
    kind: u32,
    /// The component count; zero disables the attribute.
    size: u32,
}

/// The transform stage a title uploads: its program, its constants, and
/// where execution starts.
#[derive(Debug, Clone)]
struct TransformState {
    /// Instruction slots, four dwords each, as uploaded.
    program: Vec<[u32; 4]>,
    /// The slot the next program dword lands in, counted in dwords.
    program_load: u32,
    /// The slot execution begins at.
    program_start: u32,
    /// The constant bank, four floats each.
    constants: Vec<[f32; 4]>,
    /// The constant the next upload lands in, counted in dwords.
    constant_load: u32,
    /// Whether the transform stage runs a program rather than the fixed
    /// pipeline.
    programmed: bool,
}

impl TransformState {
    /// The program from its start slot, empty when the start is past its
    /// end — a guest may set any start it likes.
    fn program_from_start(&self) -> &[[u32; 4]] {
        self.program.get(self.program_start as usize..).unwrap_or(&[])
    }
}

impl Default for TransformState {
    fn default() -> Self {
        Self {
            program: vec![[0; 4]; TRANSFORM_PROGRAM_SLOTS],
            program_load: 0,
            program_start: 0,
            constants: vec![[0.0; 4]; TRANSFORM_CONSTANT_SLOTS],
            constant_load: 0,
            programmed: false,
        }
    }
}

/// The vertex pipeline's state between `SET_BEGIN_END` pairs.
#[derive(Debug, Default, Clone)]
struct VertexState {
    /// The primitive type a begin selected, or `None` between primitives.
    primitive: Option<u32>,
    /// The declared layout of the sixteen vertex attributes.
    formats: [AttributeFormat; 16],
    /// Dwords received through `INLINE_ARRAY` for the current primitive.
    inline: Vec<u32>,
    /// Vertex indices received through `ARRAY_ELEMENT` for the current
    /// primitive, when the title draws from arrays in memory instead.
    elements: Vec<u32>,
    /// Each attribute's byte offset inside the vertex context DMA.
    array_offsets: [u32; 16],
    /// Each attribute's stride in bytes, from its format word.
    array_strides: [u32; 16],
    /// The composite (model-view-projection) matrix the fixed pipeline
    /// transforms by, in the order the title uploads it.
    composite: [f32; 16],
    /// The viewport's scale, as programmed. A title reaches its geometry
    /// through a transform program, which receives the same scale and
    /// offset as constants and applies them itself; these are recorded for
    /// the fixed pipeline, which no title on this path uses.
    viewport_scale: [f32; 4],
    /// The viewport's offset, as programmed.
    viewport_offset: [f32; 4],
}

/// When a color surface was last drawn into and last cleared.
#[derive(Debug, Default, Clone, Copy)]
struct SurfaceHistory {
    /// The operation at which a triangle last wrote to it.
    drawn: u64,
    /// The operation at which a clear last wiped it.
    cleared: u64,
    /// Its geometry, as `(pitch, width, height)`.
    geometry: (u32, u32, u32),
    /// Pixels drawn into it since its last clear. A frame the title
    /// finished has content; one it has only started has almost none.
    blended: u64,
}

/// The attribute registers a vertex starts from.
///
/// A mesh need not carry every attribute, and the ones it leaves out are
/// not zero: a vertex with no color of its own is white, which is what
/// makes its texture show through unchanged.
fn default_attributes() -> Vec<[f32; 4]> {
    let mut attributes = vec![[0.0, 0.0, 0.0, 1.0]; crate::INPUT_REGISTERS];
    attributes[ATTRIBUTE_DIFFUSE] = [1.0; 4];
    attributes[ATTRIBUTE_SPECULAR] = [1.0; 4];
    attributes
}

/// The engine's per-channel decode state.
#[derive(Debug, Default)]
struct ChannelState {
    /// The object handle bound to each of the eight subchannels.
    subchannel_handles: [u32; 8],
    /// The semaphore context-DMA object, once bound.
    semaphore: Option<DmaObject>,
    /// The current semaphore byte offset.
    semaphore_offset: u32,
    /// The render-target state.
    surface: SurfaceState,
    /// The vertex pipeline state.
    vertex: VertexState,
    /// The transform stage's program and constants.
    transform: TransformState,
    /// Whether blending is enabled.
    blend: bool,
    /// Whether a fragment is compared against the depth surface.
    depth_test: bool,
    /// Whether a passing fragment updates the depth surface.
    depth_write: bool,
    /// The depth comparison the title selected.
    depth_function: u32,
    /// The blend factors, and whether blending runs.
    blend_source: u32,
    blend_destination: u32,
    /// Whether facing is tested, which face is discarded, and which
    /// winding counts as front.
    cull: bool,
    cull_face: u32,
    front_face: u32,
    /// The alpha comparison a fragment must pass.
    alpha_test: bool,
    alpha_function: u32,
    alpha_reference: u32,
    /// The first texture unit's state.
    texture: TextureState,
    /// The context-DMA objects textures address through (`A` and `B`).
    texture_dma: [Option<DmaObject>; 2],
    /// The context-DMA objects vertex arrays address through (`A` and `B`).
    vertex_dma: [Option<DmaObject>; 2],
}

/// Aggregate statistics for diagnostics.
#[derive(Debug, Default, Clone)]
pub struct PusherStats {
    /// Total method dwords consumed.
    pub method_dwords: u64,
    /// Total submissions processed.
    pub submissions: u64,
    /// Total semaphore releases written to memory.
    pub semaphore_releases: u64,
    /// Color-surface clears applied to guest memory.
    pub surface_clears: u64,
    /// Pixels written by those clears.
    pub cleared_pixels: u64,
    /// Triangles the rasterizer filled.
    pub triangles: u64,
    /// Pixels those triangles wrote.
    pub shaded_pixels: u64,
    /// Primitives skipped because their vertex layout is not modeled.
    pub skipped_primitives: u64,
    /// Primitives drawn with a texture bound.
    pub textured_primitives: u64,
    /// Primitives whose texture format this engine cannot sample.
    pub unsupported_textures: u64,
    /// Submissions abandoned mid-walk (bad word or unreadable memory).
    pub aborted: u64,
}

/// One submission's fixed parameters.
struct SubmitContext {
    channel: u32,
    pramin: u32,
    ramht_raw: u32,
}

/// A bound texture the rasterizer samples through guest memory.
///
/// Two layouts cover a title's own surfaces and its uncompressed art: a
/// linear one, whose rows sit a programmed pitch apart, and a swizzled one,
/// whose texels are ordered by interleaving the bits of their coordinates
/// (Morton order), which is how the hardware stores a power-of-two texture.
struct MemoryTexture<'a> {
    memory: &'a dyn Nv2aMemory,
    base: u32,
    pitch: u32,
    width: u32,
    height: u32,
    layout: TextureLayout,
    /// Whether the format's alpha channel is meaningful.
    has_alpha: bool,
    /// One past the last byte the texture's own object covers. A sample
    /// beyond it reads as transparent rather than as another object's
    /// memory.
    end: u32,
}

/// How a bound texture's texels are arranged in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureLayout {
    /// Rows a programmed pitch apart.
    Linear,
    /// Texels in Morton order, as the hardware stores a power-of-two image.
    Swizzled,
    /// Four-by-four blocks of `DXT1` color.
    Dxt1,
    /// Blocks of explicit alpha followed by `DXT1` color.
    Dxt3,
    /// Blocks of interpolated alpha followed by `DXT1` color.
    Dxt5,
}

/// Interleaves the low bits of two coordinates into a Morton index.
fn swizzle_index(x: u32, y: u32, width_bits: u32, height_bits: u32) -> u32 {
    let mut index = 0;
    let mut bit = 0;
    let (mut x_bit, mut y_bit) = (0, 0);
    while x_bit < width_bits || y_bit < height_bits {
        if x_bit < width_bits {
            index |= ((x >> x_bit) & 1) << bit;
            x_bit += 1;
            bit += 1;
        }
        if y_bit < height_bits {
            index |= ((y >> y_bit) & 1) << bit;
            y_bit += 1;
            bit += 1;
        }
    }
    index
}

impl MemoryTexture<'_> {
    /// Reads the block containing a texel, as two dwords at `offset`.
    fn block(&self, x: u32, y: u32, block_bytes: u32, offset: u32) -> [u32; 2] {
        let blocks_across = self.width.div_ceil(4);
        let index = (y / 4) * blocks_across + (x / 4);
        let address = self.base.wrapping_add(index * block_bytes).wrapping_add(offset);
        [self.read(address), self.read(address.wrapping_add(4))]
    }

    /// Reads one dword of the texture, refusing to leave its object.
    fn read(&self, address: u32) -> u32 {
        if address < self.base || address >= self.end {
            return 0;
        }
        self.memory.read_dword(address).unwrap_or(0)
    }
}

impl crate::TextureSource for MemoryTexture<'_> {
    fn texel(&self, x: u32, y: u32) -> u32 {
        use crate::texture::{DXT_ALPHA_BLOCK_BYTES, DXT1_BLOCK_BYTES};

        let address = match self.layout {
            TextureLayout::Swizzled => {
                let index =
                    swizzle_index(x, y, self.width.trailing_zeros(), self.height.trailing_zeros());
                self.base.wrapping_add(index * 4)
            }
            TextureLayout::Linear => {
                self.base.wrapping_add(y.wrapping_mul(self.pitch)).wrapping_add(x * 4)
            }
            TextureLayout::Dxt1 => {
                let block = self.block(x, y, DXT1_BLOCK_BYTES, 0);
                return crate::dxt1_texel(block, x, y);
            }
            // The color half of an alpha-carrying block follows its alpha.
            TextureLayout::Dxt3 => {
                let alpha = crate::dxt3_alpha(self.block(x, y, DXT_ALPHA_BLOCK_BYTES, 0), x, y);
                let color =
                    crate::dxt_opaque_texel(self.block(x, y, DXT_ALPHA_BLOCK_BYTES, 8), x, y);
                return (color & 0x00FF_FFFF) | (alpha << 24);
            }
            TextureLayout::Dxt5 => {
                let alpha = crate::dxt5_alpha(self.block(x, y, DXT_ALPHA_BLOCK_BYTES, 0), x, y);
                let color =
                    crate::dxt_opaque_texel(self.block(x, y, DXT_ALPHA_BLOCK_BYTES, 8), x, y);
                return (color & 0x00FF_FFFF) | (alpha << 24);
            }
        };
        let texel = self.read(address);
        if self.has_alpha { texel } else { texel | 0xFF00_0000 }
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }
}

/// The DMA pusher: walks submitted command ranges and applies effects.
#[derive(Debug, Default)]
pub struct PushbufferEngine {
    channels: HashMap<u32, ChannelState>,
    /// Per-method dword counts, keyed by (subchannel-bound handle, method).
    method_counts: HashMap<(u32, u16), u64>,
    /// Pixels written per color surface, for diagnostics: it answers where
    /// a frame's drawing actually landed.
    pixels_by_target: HashMap<u32, u64>,
    /// When each color surface was last drawn into and last cleared, on a
    /// counter of graphics operations. A surface whose last draw came after
    /// its last clear holds a finished frame; one cleared since is the
    /// buffer the title has started drawing the next frame into.
    surface_history: HashMap<u32, SurfaceHistory>,
    /// The operation counter those timestamps come from.
    operations: u64,
    /// The texture most recently sampled, as `(state, context object)`.
    /// Dumping it answers whether a black frame is bad addressing or a
    /// texture that is genuinely empty.
    last_texture: Option<(TextureState, Option<DmaObject>)>,
    /// The color surface the last triangle landed in. A capture reads this
    /// rather than the encoder's programmed frame buffer: the title's own
    /// buffer flip is not modeled, so the surface it just drew into is the
    /// truthful "latest frame".
    last_draw_target: u32,
    /// That surface's geometry (pitch, width, height).
    last_draw_geometry: (u32, u32, u32),
    /// The surface drawn into before the current one. With double
    /// buffering that is the finished frame: the title is busy drawing the
    /// next one over the buffer it just presented.
    previous_draw: Option<(u32, u32, u32, u32)>,
    /// A frame finished since the last time a caller asked: the title
    /// moved on to another surface, so this one is complete.
    completed_frame: Option<(u32, u32, u32, u32)>,
    stats: PusherStats,
}

/// `NV_PFIFO_RAMHT` decoding: the hash-table offset inside instance memory
/// and its size in bytes.
fn ramht_layout(ramht_raw: u32) -> (u32, u32) {
    let offset = (ramht_raw & 0x1F0) << 8;
    let size = 1_u32 << (((ramht_raw >> 16) & 0x3) + 12);
    (offset, size)
}

/// The hardware `RAMHT` hash: fold the handle by the table's index width,
/// then mix the channel identifier into the top bits.
fn ramht_hash(handle: u32, index_bits: u32, channel: u32) -> u32 {
    let mut hash = 0_u32;
    let mut value = handle;
    let mask = (1_u32 << index_bits) - 1;
    while value != 0 {
        hash ^= value & mask;
        value >>= index_bits;
    }
    hash ^ ((channel & 0xF) << (index_bits - 4))
}

/// The most distinct methods the census records before it stops growing.
const MAX_CENSUS_ENTRIES: usize = 4096;

/// The largest texture edge the hardware addresses; a format word can ask
/// for more, and a sampler built from one would walk gigabytes.
const MAX_TEXTURE_DIMENSION: u32 = 4096;

/// The largest surface edge a title can draw into, for the same reason.
const MAX_SURFACE_DIMENSION: u32 = 4096;

/// The narrowest surface a capture treats as a displayable frame.
const PRESENTABLE_WIDTH: u32 = 512;

/// The most inline vertex dwords one primitive may carry.
const MAX_INLINE_DWORDS: usize = 1 << 20;

/// Per-submission dword budget: a runaway or circular pushbuffer must not
/// hang the emulator.
const MAX_DWORDS_PER_SUBMIT: u32 = 4 * 1024 * 1024;

/// The `SET_OBJECT` method (binds a handle to a subchannel).
const METHOD_SET_OBJECT: u16 = 0x0000;
/// Kelvin `SET_CONTEXT_DMA_COLOR`: names the object the color surface's
/// offset is relative to.
const METHOD_SET_CONTEXT_DMA_COLOR: u16 = 0x0190;
/// Kelvin `SET_CONTEXT_DMA_SEMAPHORE`.
///
/// Confirmed against the retail stream by what the object *is*: the title
/// binds a 32-byte object here whose base is the very block its pushbuffer
/// wait polls for progress, while `0x01A0` (the report object) binds the
/// whole of RAM. Writing fences to the report object instead leaves that
/// wait spinning forever.
const METHOD_SET_CONTEXT_DMA_SEMAPHORE: u16 = 0x01A4;
/// Kelvin `SET_SURFACE_CLIP_HORIZONTAL`: left edge and width.
const METHOD_SET_SURFACE_CLIP_HORIZONTAL: u16 = 0x0200;
/// Kelvin `SET_SURFACE_CLIP_VERTICAL`: top edge and height.
const METHOD_SET_SURFACE_CLIP_VERTICAL: u16 = 0x0204;
/// Kelvin `SET_SURFACE_PITCH`: color pitch low, zeta pitch high.
const METHOD_SET_SURFACE_PITCH: u16 = 0x020C;
/// Kelvin `SET_SURFACE_COLOR_OFFSET`.
const METHOD_SET_SURFACE_COLOR_OFFSET: u16 = 0x0210;
/// Kelvin `SET_ZSTENCIL_CLEAR_VALUE`.
const METHOD_SET_ZSTENCIL_CLEAR_VALUE: u16 = 0x1D8C;
/// Kelvin `SET_COLOR_CLEAR_VALUE`.
const METHOD_SET_COLOR_CLEAR_VALUE: u16 = 0x1D90;
/// The `CLEAR_SURFACE` bits selecting depth and stencil.
const CLEAR_DEPTH_MASK: u32 = 0x03;
/// Kelvin `CLEAR_SURFACE`: the buffers a clear touches.
const METHOD_CLEAR_SURFACE: u16 = 0x1D94;
/// Kelvin `SET_CLEAR_RECT_HORIZONTAL`: left and right edges.
const METHOD_SET_CLEAR_RECT_HORIZONTAL: u16 = 0x1D98;
/// Kelvin `SET_CLEAR_RECT_VERTICAL`: top and bottom edges.
const METHOD_SET_CLEAR_RECT_VERTICAL: u16 = 0x1D9C;
/// The `CLEAR_SURFACE` bits selecting the color channels.
const CLEAR_COLOR_MASK: u32 = 0xF0;
/// Kelvin `SET_VIEWPORT_OFFSET`: four floats, the first two of which map
/// clip space onto the surface.
const METHOD_SET_VIEWPORT_OFFSET: u16 = 0x0A20;
/// Kelvin `SET_VIEWPORT_SCALE`: four floats.
const METHOD_SET_VIEWPORT_SCALE: u16 = 0x0AF0;
/// Kelvin `SET_BEGIN_END`: a primitive type, or zero to end one.
const METHOD_SET_BEGIN_END: u16 = 0x17FC;
/// Kelvin `INLINE_ARRAY`: vertex data inline in the pushbuffer.
const METHOD_INLINE_ARRAY: u16 = 0x1818;
/// The first `SET_VERTEX_DATA_ARRAY_FORMAT` method; sixteen follow.
const METHOD_SET_VERTEX_DATA_ARRAY_FORMAT: u16 = 0x1760;
/// Kelvin `SET_COMPOSITE_MATRIX`: sixteen floats, the fixed pipeline's
/// model-view-projection product.
const METHOD_SET_COMPOSITE_MATRIX: u16 = 0x0680;
/// The last of its sixteen methods.
const COMPOSITE_MATRIX_LAST: u16 = 0x06BC;
/// The first `SET_VERTEX_DATA_ARRAY_OFFSET` method; sixteen follow.
const METHOD_SET_VERTEX_DATA_ARRAY_OFFSET: u16 = 0x1720;
/// The last of them.
const VERTEX_ARRAY_OFFSET_LAST: u16 = 0x175C;
/// Kelvin `ARRAY_ELEMENT16`: two vertex indices per dword.
const METHOD_ARRAY_ELEMENT16: u16 = 0x1800;
/// Kelvin `ARRAY_ELEMENT32`: one vertex index per dword.
const METHOD_ARRAY_ELEMENT32: u16 = 0x1808;
/// Kelvin `SET_CONTEXT_DMA_VERTEX_A`.
const METHOD_SET_CONTEXT_DMA_VERTEX_A: u16 = 0x019C;
/// Kelvin `SET_CONTEXT_DMA_VERTEX_B`.
const METHOD_SET_CONTEXT_DMA_VERTEX_B: u16 = 0x01A0;
/// The offset bit selecting the second vertex context.
const VERTEX_ARRAY_CONTEXT_B: u32 = 0x8000_0000;
/// The most vertex indices one primitive may carry.
const MAX_ELEMENTS: usize = 1 << 18;
/// The vertex attribute carrying position.
const ATTRIBUTE_POSITION: usize = 0;
/// The vertex attribute carrying the diffuse color.
const ATTRIBUTE_DIFFUSE: usize = 3;
/// The vertex attribute carrying the specular color.
const ATTRIBUTE_SPECULAR: usize = 4;
/// The attribute component type for a packed `D3DCOLOR` dword (ARGB).
const ATTRIBUTE_TYPE_D3DCOLOR: u32 = 0;
/// The attribute component type for 32-bit floats.
const ATTRIBUTE_TYPE_FLOAT: u32 = 2;
/// The attribute component type for four packed bytes in RGBA order.
const ATTRIBUTE_TYPE_UBYTE_RGBA: u32 = 3;
/// The last method of the four-float viewport offset.
const VIEWPORT_OFFSET_LAST: u16 = METHOD_SET_VIEWPORT_OFFSET + 12;
/// The last method of the four-float viewport scale.
const VIEWPORT_SCALE_LAST: u16 = METHOD_SET_VIEWPORT_SCALE + 12;
/// The last of the sixteen vertex-attribute format methods.
const VERTEX_FORMAT_LAST: u16 = METHOD_SET_VERTEX_DATA_ARRAY_FORMAT + 15 * 4;
/// Instruction slots the transform program holds.
const TRANSFORM_PROGRAM_SLOTS: usize = 136;
/// Constant slots the transform stage holds.
const TRANSFORM_CONSTANT_SLOTS: usize = 192;
/// The first `SET_TRANSFORM_PROGRAM` method; thirty-two follow, each one
/// dword of the program at the load pointer.
const METHOD_SET_TRANSFORM_PROGRAM: u16 = 0x0B00;
/// The last of them.
const TRANSFORM_PROGRAM_LAST: u16 = 0x0B7C;
/// The first `SET_TRANSFORM_CONSTANT` method; thirty-two follow.
const METHOD_SET_TRANSFORM_CONSTANT: u16 = 0x0B80;
/// The last of them.
const TRANSFORM_CONSTANT_LAST: u16 = 0x0BFC;
/// Kelvin `SET_TRANSFORM_EXECUTION_MODE`: fixed pipeline or program.
const METHOD_SET_TRANSFORM_EXECUTION_MODE: u16 = 0x1E94;
/// Kelvin `SET_TRANSFORM_PROGRAM_LOAD`: where uploaded instructions land.
const METHOD_SET_TRANSFORM_PROGRAM_LOAD: u16 = 0x1E9C;
/// Kelvin `SET_TRANSFORM_PROGRAM_START`: where execution begins.
const METHOD_SET_TRANSFORM_PROGRAM_START: u16 = 0x1EA0;
/// Kelvin `SET_TRANSFORM_CONSTANT_LOAD`: where uploaded constants land.
const METHOD_SET_TRANSFORM_CONSTANT_LOAD: u16 = 0x1EA4;
/// The execution-mode field selecting a program over the fixed pipeline.
const TRANSFORM_MODE_PROGRAM: u32 = 2;

/// Kelvin `SET_BLEND_ENABLE`.
const METHOD_SET_BLEND_ENABLE: u16 = 0x0304;
/// Kelvin `SET_ALPHA_TEST_ENABLE`.
const METHOD_SET_ALPHA_TEST_ENABLE: u16 = 0x0300;
/// Kelvin `SET_ALPHA_FUNC`.
const METHOD_SET_ALPHA_FUNC: u16 = 0x033C;
/// Kelvin `SET_ALPHA_REF`.
const METHOD_SET_ALPHA_REF: u16 = 0x0340;
/// Kelvin `SET_BLEND_FUNC_SFACTOR`.
const METHOD_SET_BLEND_FUNC_SFACTOR: u16 = 0x0344;
/// Kelvin `SET_BLEND_FUNC_DFACTOR`.
const METHOD_SET_BLEND_FUNC_DFACTOR: u16 = 0x0348;
/// Kelvin `SET_CULL_FACE_ENABLE`.
const METHOD_SET_CULL_FACE_ENABLE: u16 = 0x0308;
/// Kelvin `SET_CULL_FACE`.
const METHOD_SET_CULL_FACE: u16 = 0x039C;
/// Kelvin `SET_FRONT_FACE`.
const METHOD_SET_FRONT_FACE: u16 = 0x03A0;
/// Kelvin `SET_DEPTH_TEST_ENABLE`.
const METHOD_SET_DEPTH_TEST_ENABLE: u16 = 0x030C;
/// Kelvin `SET_DEPTH_FUNC`.
const METHOD_SET_DEPTH_FUNC: u16 = 0x0354;
/// Kelvin `SET_DEPTH_MASK`: whether depth writes reach the surface.
const METHOD_SET_DEPTH_MASK: u16 = 0x035C;
/// Kelvin `SET_SURFACE_ZETA_OFFSET`.
const METHOD_SET_SURFACE_ZETA_OFFSET: u16 = 0x0214;
/// Kelvin `SET_TEXTURE_OFFSET` for unit zero; units are 64 bytes apart.
const METHOD_SET_TEXTURE_OFFSET: u16 = 0x1B00;
/// Kelvin `SET_TEXTURE_FORMAT` for unit zero.
const METHOD_SET_TEXTURE_FORMAT: u16 = 0x1B04;
/// Kelvin `SET_TEXTURE_CONTROL0` for unit zero: the enable bit lives here.
const METHOD_SET_TEXTURE_CONTROL0: u16 = 0x1B0C;
/// Kelvin `SET_TEXTURE_CONTROL1` for unit zero: the row pitch, in the high
/// half of the word.
const METHOD_SET_TEXTURE_CONTROL1: u16 = 0x1B10;
/// Kelvin `SET_TEXTURE_IMAGE_RECT` for unit zero: width and height a linear
/// format cannot express as logarithms.
const METHOD_SET_TEXTURE_IMAGE_RECT: u16 = 0x1B1C;
/// Kelvin `SET_CONTEXT_DMA_A`, the first texture context.
const METHOD_SET_CONTEXT_DMA_A: u16 = 0x0184;
/// Kelvin `SET_CONTEXT_DMA_B`, the second texture context.
const METHOD_SET_CONTEXT_DMA_B: u16 = 0x0188;
/// `SET_TEXTURE_CONTROL0` enable bit.
const TEXTURE_ENABLE: u32 = 0x4000_0000;
/// The texture color format for linear 8-bit ARGB.
const TEXTURE_FORMAT_LINEAR_A8R8G8B8: u32 = 0x12;
/// The texture color format for linear 8-bit XRGB (alpha reads as one).
const TEXTURE_FORMAT_LINEAR_X8R8G8B8: u32 = 0x11;
/// The texture color format for swizzled 8-bit ARGB.
const TEXTURE_FORMAT_SWIZZLED_A8R8G8B8: u32 = 0x06;
/// The texture color format for `DXT1` blocks.
const TEXTURE_FORMAT_DXT1: u32 = 0x0C;
/// The texture color format for `DXT3` blocks.
const TEXTURE_FORMAT_DXT3: u32 = 0x0E;
/// The texture color format for `DXT5` blocks.
const TEXTURE_FORMAT_DXT5: u32 = 0x0F;
/// The vertex attribute carrying the first texture coordinate set.
const ATTRIBUTE_TEXCOORD0: usize = 9;

/// `SET_BEGIN_END` primitive types.
const PRIMITIVE_TRIANGLES: u32 = 5;
const PRIMITIVE_TRIANGLE_STRIP: u32 = 6;
const PRIMITIVE_TRIANGLE_FAN: u32 = 7;
const PRIMITIVE_QUADS: u32 = 8;
/// Kelvin `SET_SEMAPHORE_OFFSET`.
const METHOD_SET_SEMAPHORE_OFFSET: u16 = 0x1D6C;
/// Kelvin `BACK_END_WRITE_SEMAPHORE_RELEASE`.
const METHOD_BACK_END_WRITE_SEMAPHORE_RELEASE: u16 = 0x1D70;

impl PushbufferEngine {
    /// Resolves an object handle to its DMA object through `RAMHT`.
    ///
    /// `pramin` is the physical base of instance memory; `ramht_raw` is the
    /// guest-programmed `NV_PFIFO_RAMHT` register value.
    fn resolve_dma_object(
        memory: &dyn Nv2aMemory,
        pramin: u32,
        ramht_raw: u32,
        channel: u32,
        handle: u32,
    ) -> Option<DmaObject> {
        let (ramht_offset, ramht_size) = ramht_layout(ramht_raw);
        let entries = ramht_size / 8;
        let index_bits = entries.trailing_zeros();
        if index_bits < 4 {
            return None;
        }
        let hash = ramht_hash(handle, index_bits, channel) % entries;
        let entry = pramin.checked_add(ramht_offset)?.checked_add(hash * 8)?;
        let stored_handle = memory.read_dword(entry)?;
        if stored_handle != handle {
            return None;
        }
        let context = memory.read_dword(entry + 4)?;
        let instance = (context & 0xFFFF) << 4;
        let base = pramin.checked_add(instance)?;
        let d0 = memory.read_dword(base)?;
        let limit = memory.read_dword(base + 4)?;
        let d2 = memory.read_dword(base + 8)?;
        Some(DmaObject {
            class: d0 & 0xFFF,
            limit,
            base: (d2 & 0xFFFF_F000) | ((d0 >> 20) & 0xFFF),
        })
    }

    /// Walks one submitted range, applying method effects.
    ///
    /// `get` and `put` are physical pushbuffer addresses; `pramin` is the
    /// physical base of instance memory and `ramht_raw` the latched
    /// `NV_PFIFO_RAMHT` value. Returns the final `GET` (equal to `put` on a
    /// clean walk; the abandonment point otherwise).
    pub fn submit(
        &mut self,
        memory: &dyn Nv2aMemory,
        channel: u32,
        pramin: u32,
        ramht_raw: u32,
        get: u32,
        put: u32,
    ) -> u32 {
        let context = SubmitContext { channel, pramin, ramht_raw };
        self.submit_inner(memory, &context, get, put)
    }

    fn submit_inner(
        &mut self,
        memory: &dyn Nv2aMemory,
        context: &SubmitContext,
        get: u32,
        put: u32,
    ) -> u32 {
        self.stats.submissions += 1;
        let mut cursor = get & !3;
        let target = put & !3;
        let mut budget = MAX_DWORDS_PER_SUBMIT;
        let mut call_return: Option<u32> = None;

        while cursor != target {
            if budget == 0 {
                tracing::warn!(
                    cursor = format_args!("{cursor:#010x}"),
                    "pushbuffer walk exceeded its dword budget"
                );
                self.stats.aborted += 1;
                return cursor;
            }
            budget -= 1;

            let Some(word) = memory.read_dword(cursor) else {
                tracing::warn!(
                    cursor = format_args!("{cursor:#010x}"),
                    "pushbuffer walk read unmapped memory"
                );
                self.stats.aborted += 1;
                return cursor;
            };
            cursor = cursor.wrapping_add(4);

            // Control words, per the hardware pusher's precedence.
            if word & 0xE000_0003 == 0x2000_0000 {
                cursor = word & 0x1FFF_FFFC;
                continue;
            }
            if word & 0x3 == 0x1 {
                cursor = word & 0xFFFF_FFFC;
                continue;
            }
            if word & 0x3 == 0x2 {
                call_return = Some(cursor);
                cursor = word & 0xFFFF_FFFC;
                continue;
            }
            if word == 0x0002_0000 {
                let Some(back) = call_return.take() else {
                    tracing::warn!("pushbuffer return without a call");
                    self.stats.aborted += 1;
                    return cursor;
                };
                cursor = back;
                continue;
            }

            let non_increasing = match word & 0xE003_0003 {
                0x0000_0000 => false,
                0x4000_0000 => true,
                _ => {
                    tracing::warn!(
                        word = format_args!("{word:#010x}"),
                        "unrecognized pushbuffer control word"
                    );
                    self.stats.aborted += 1;
                    return cursor.wrapping_sub(4);
                }
            };
            let count = (word >> 18) & 0x7FF;
            let subchannel = ((word >> 13) & 0x7) as usize;
            let mut method = (word & 0x1FFC) as u16;

            // Every argument dword costs budget, not just the header:
            // one header can carry two thousand of them, and a circular
            // pushbuffer of them would otherwise run for hours.
            budget = budget.saturating_sub(count);
            for _ in 0..count {
                let Some(argument) = memory.read_dword(cursor) else {
                    self.stats.aborted += 1;
                    return cursor;
                };
                cursor = cursor.wrapping_add(4);
                self.stats.method_dwords += 1;
                self.apply_method(memory, context, subchannel, method, argument);
                if !non_increasing {
                    method = method.wrapping_add(4);
                }
            }
        }
        cursor
    }

    /// Applies one decoded method.
    fn apply_method(
        &mut self,
        memory: &dyn Nv2aMemory,
        context: &SubmitContext,
        subchannel: usize,
        method: u16,
        argument: u32,
    ) {
        let channel = context.channel;
        let state = self.channels.entry(channel).or_default();
        let bound = state.subchannel_handles[subchannel & 7];
        // The census is keyed partly by an object handle the guest picks,
        // so it is capped: a stream that binds a fresh handle per method
        // would otherwise grow it until memory ran out.
        if self.method_counts.len() < MAX_CENSUS_ENTRIES
            || self.method_counts.contains_key(&(bound, method))
        {
            *self.method_counts.entry((bound, method)).or_insert(0) += 1;
        }
        tracing::trace!(
            channel,
            subchannel,
            method = format_args!("{method:#06x}"),
            argument = format_args!("{argument:#010x}"),
            "nv2a method"
        );
        match method {
            METHOD_SET_OBJECT => {
                state.subchannel_handles[subchannel & 7] = argument;
                tracing::debug!(
                    subchannel,
                    handle = format_args!("{argument:#010x}"),
                    "nv2a object bound"
                );
            }
            METHOD_SET_CONTEXT_DMA_SEMAPHORE => {
                let resolved = Self::resolve_dma_object(
                    memory,
                    context.pramin,
                    context.ramht_raw,
                    channel,
                    argument,
                );
                let state = self.channels.entry(channel).or_default();
                state.semaphore = resolved;
                tracing::debug!(
                    handle = format_args!("{argument:#010x}"),
                    ?resolved,
                    "nv2a semaphore context bound"
                );
            }
            METHOD_SET_SEMAPHORE_OFFSET => {
                state.semaphore_offset = argument;
            }
            METHOD_SET_CONTEXT_DMA_COLOR => {
                let resolved = Self::resolve_dma_object(
                    memory,
                    context.pramin,
                    context.ramht_raw,
                    channel,
                    argument,
                );
                let state = self.channels.entry(channel).or_default();
                state.surface.color_dma = resolved;
                tracing::debug!(
                    handle = format_args!("{argument:#010x}"),
                    ?resolved,
                    "nv2a color surface context bound"
                );
            }
            METHOD_SET_SURFACE_CLIP_HORIZONTAL => {
                state.surface.clip_x = argument & 0xFFFF;
                state.surface.clip_width = argument >> 16;
            }
            METHOD_SET_SURFACE_CLIP_VERTICAL => {
                state.surface.clip_y = argument & 0xFFFF;
                state.surface.clip_height = argument >> 16;
            }
            METHOD_SET_SURFACE_PITCH => {
                state.surface.color_pitch = argument & 0xFFFF;
                state.surface.zeta_pitch = argument >> 16;
            }
            METHOD_SET_SURFACE_ZETA_OFFSET => {
                state.surface.zeta_offset = argument;
            }
            METHOD_SET_ALPHA_TEST_ENABLE => {
                state.alpha_test = argument != 0;
            }
            METHOD_SET_ALPHA_FUNC => {
                state.alpha_function = argument;
            }
            METHOD_SET_ALPHA_REF => {
                state.alpha_reference = argument;
            }
            METHOD_SET_BLEND_FUNC_SFACTOR => {
                state.blend_source = argument;
            }
            METHOD_SET_BLEND_FUNC_DFACTOR => {
                state.blend_destination = argument;
            }
            METHOD_SET_CULL_FACE_ENABLE => {
                state.cull = argument != 0;
            }
            METHOD_SET_CULL_FACE => {
                state.cull_face = argument;
            }
            METHOD_SET_FRONT_FACE => {
                state.front_face = argument;
            }
            METHOD_SET_DEPTH_TEST_ENABLE => {
                state.depth_test = argument != 0;
            }
            METHOD_SET_DEPTH_MASK => {
                state.depth_write = argument != 0;
            }
            METHOD_SET_DEPTH_FUNC => {
                state.depth_function = argument;
            }
            METHOD_SET_SURFACE_COLOR_OFFSET => {
                state.surface.color_offset = argument;
            }
            METHOD_SET_COLOR_CLEAR_VALUE => {
                state.surface.clear_color = argument;
            }
            METHOD_SET_ZSTENCIL_CLEAR_VALUE => {
                state.surface.clear_zstencil = argument;
            }
            METHOD_SET_CLEAR_RECT_HORIZONTAL => {
                state.surface.clear_x = (argument & 0xFFFF, argument >> 16);
            }
            METHOD_SET_CLEAR_RECT_VERTICAL => {
                state.surface.clear_y = (argument & 0xFFFF, argument >> 16);
            }
            METHOD_CLEAR_SURFACE => {
                let surface = self.channels.entry(channel).or_default().surface;
                if argument & CLEAR_COLOR_MASK != 0 {
                    self.clear_color_surface(memory, &surface);
                }
                if argument & CLEAR_DEPTH_MASK != 0 {
                    // A depth clear is the same rectangle fill against the
                    // depth surface; without it every fragment would be
                    // compared against whatever the memory last held.
                    Self::clear_zeta_surface(memory, &surface);
                }
            }
            METHOD_SET_VIEWPORT_OFFSET..=VIEWPORT_OFFSET_LAST => {
                let index = ((method - METHOD_SET_VIEWPORT_OFFSET) / 4) as usize;
                state.vertex.viewport_offset[index & 3] = f32::from_bits(argument);
            }
            METHOD_SET_VIEWPORT_SCALE..=VIEWPORT_SCALE_LAST => {
                let index = ((method - METHOD_SET_VIEWPORT_SCALE) / 4) as usize;
                state.vertex.viewport_scale[index & 3] = f32::from_bits(argument);
            }
            METHOD_SET_VERTEX_DATA_ARRAY_FORMAT..=VERTEX_FORMAT_LAST => {
                let index = ((method - METHOD_SET_VERTEX_DATA_ARRAY_FORMAT) / 4) as usize;
                state.vertex.formats[index & 15] =
                    AttributeFormat { kind: argument & 0xF, size: (argument >> 4) & 0xF };
                state.vertex.array_strides[index & 15] = argument >> 8;
            }
            METHOD_SET_VERTEX_DATA_ARRAY_OFFSET..=VERTEX_ARRAY_OFFSET_LAST => {
                let index = ((method - METHOD_SET_VERTEX_DATA_ARRAY_OFFSET) / 4) as usize;
                state.vertex.array_offsets[index & 15] = argument;
            }
            METHOD_SET_COMPOSITE_MATRIX..=COMPOSITE_MATRIX_LAST => {
                let index = ((method - METHOD_SET_COMPOSITE_MATRIX) / 4) as usize;
                state.vertex.composite[index & 15] = f32::from_bits(argument);
            }
            METHOD_ARRAY_ELEMENT16 => {
                // Two indices per dword, low half first.
                if state.vertex.primitive.is_some()
                    && state.vertex.elements.len() + 2 <= MAX_ELEMENTS
                {
                    state.vertex.elements.push(argument & 0xFFFF);
                    state.vertex.elements.push(argument >> 16);
                }
            }
            METHOD_ARRAY_ELEMENT32 => {
                if state.vertex.primitive.is_some() && state.vertex.elements.len() < MAX_ELEMENTS {
                    state.vertex.elements.push(argument);
                }
            }
            METHOD_SET_CONTEXT_DMA_VERTEX_A | METHOD_SET_CONTEXT_DMA_VERTEX_B => {
                let resolved = Self::resolve_dma_object(
                    memory,
                    context.pramin,
                    context.ramht_raw,
                    channel,
                    argument,
                );
                let state = self.channels.entry(channel).or_default();
                state.vertex_dma[usize::from(method == METHOD_SET_CONTEXT_DMA_VERTEX_B)] = resolved;
            }
            METHOD_SET_TRANSFORM_PROGRAM..=TRANSFORM_PROGRAM_LAST => {
                let slot = (state.transform.program_load / 4) as usize;
                let component = (state.transform.program_load % 4) as usize;
                if let Some(instruction) = state.transform.program.get_mut(slot) {
                    instruction[component] = argument;
                }
                state.transform.program_load = state.transform.program_load.wrapping_add(1);
            }
            METHOD_SET_TRANSFORM_CONSTANT..=TRANSFORM_CONSTANT_LAST => {
                let slot = (state.transform.constant_load / 4) as usize;
                let component = (state.transform.constant_load % 4) as usize;
                if let Some(constant) = state.transform.constants.get_mut(slot) {
                    constant[component] = f32::from_bits(argument);
                }
                state.transform.constant_load = state.transform.constant_load.wrapping_add(1);
            }
            METHOD_SET_TRANSFORM_EXECUTION_MODE => {
                state.transform.programmed = argument & 0x3 == TRANSFORM_MODE_PROGRAM;
            }
            METHOD_SET_TRANSFORM_PROGRAM_LOAD => {
                state.transform.program_load = argument.wrapping_mul(4);
            }
            METHOD_SET_TRANSFORM_PROGRAM_START => {
                // A start beyond the program runs nothing, which is what the
                // slice below needs; it must never index past the end.
                state.transform.program_start = argument.min(TRANSFORM_PROGRAM_SLOTS as u32);
            }
            METHOD_SET_TRANSFORM_CONSTANT_LOAD => {
                // The index addresses the hardware bank directly, and a
                // program's constant field indexes the same bank.
                state.transform.constant_load = argument.wrapping_mul(4);
            }
            METHOD_SET_BLEND_ENABLE => {
                state.blend = argument != 0;
            }
            METHOD_SET_TEXTURE_OFFSET => {
                state.texture.offset = argument;
            }
            METHOD_SET_TEXTURE_FORMAT => {
                state.texture.format = argument;
            }
            METHOD_SET_TEXTURE_CONTROL0 => {
                state.texture.enabled = argument & TEXTURE_ENABLE != 0;
            }
            METHOD_SET_TEXTURE_CONTROL1 => {
                state.texture.pitch = argument >> 16;
            }
            METHOD_SET_TEXTURE_IMAGE_RECT => {
                state.texture.rect = (argument >> 16, argument & 0xFFFF);
            }
            METHOD_SET_CONTEXT_DMA_A | METHOD_SET_CONTEXT_DMA_B => {
                let resolved = Self::resolve_dma_object(
                    memory,
                    context.pramin,
                    context.ramht_raw,
                    channel,
                    argument,
                );
                let state = self.channels.entry(channel).or_default();
                state.texture_dma[usize::from(method == METHOD_SET_CONTEXT_DMA_B)] = resolved;
            }
            METHOD_SET_BEGIN_END => {
                if argument == 0 {
                    self.end_primitive(memory, channel);
                } else {
                    state.vertex.primitive = Some(argument);
                    state.vertex.inline.clear();
                    state.vertex.elements.clear();
                }
            }
            METHOD_INLINE_ARRAY => {
                // A primitive's vertex data can be long; the budget bounds a
                // runaway stream, not the geometry.
                if state.vertex.primitive.is_some() && state.vertex.inline.len() < MAX_INLINE_DWORDS
                {
                    state.vertex.inline.push(argument);
                }
            }
            METHOD_BACK_END_WRITE_SEMAPHORE_RELEASE => {
                let (target, offset) = {
                    let state = self.channels.entry(channel).or_default();
                    (state.semaphore, state.semaphore_offset)
                };
                if let Some(semaphore) = target {
                    let address = semaphore.base.wrapping_add(offset);
                    if memory.write_dword(address, argument) {
                        self.stats.semaphore_releases += 1;
                        tracing::debug!(
                            address = format_args!("{address:#010x}"),
                            value = format_args!("{argument:#010x}"),
                            "nv2a semaphore released"
                        );
                    }
                }
            }
            _ => {}
        }
    }

    /// Assembles and draws the primitive a `SET_BEGIN_END(0)` closed.
    ///
    /// Only inline vertex data with a float position attribute is modeled;
    /// anything else is counted and skipped, so what is missing stays
    /// visible in the statistics rather than quietly drawing nothing.
    fn end_primitive(&mut self, memory: &dyn Nv2aMemory, channel: u32) {
        let (primitive, vertices, surface, texture, texture_dma, pipeline) = {
            let state = self.channels.entry(channel).or_default();
            let Some(primitive) = state.vertex.primitive.take() else {
                return;
            };
            let vertices = if state.vertex.elements.is_empty() {
                Self::assemble_inline(&state.vertex, &state.transform)
            } else {
                Self::assemble_arrays(memory, &state.vertex, &state.transform, &state.vertex_dma)
            };
            state.vertex.inline.clear();
            state.vertex.elements.clear();
            let dma = state.texture_dma[(state.texture.format & 3).saturating_sub(1) as usize & 1];
            let pipeline = crate::PipelineState {
                blend: crate::BlendState {
                    enabled: state.blend,
                    source: state.blend_source,
                    destination: state.blend_destination,
                },
                depth: crate::DepthState {
                    test: state.depth_test,
                    write: state.depth_write,
                    // The hardware's comparisons are numbered from a base
                    // of `never`; a title programs `0x200 + comparison`.
                    function: state.depth_function & 0x7,
                },
                cull: crate::CullState {
                    enabled: state.cull,
                    face: state.cull_face,
                    front_face: state.front_face,
                },
                alpha: crate::AlphaTest {
                    enabled: state.alpha_test,
                    function: state.alpha_function & 0x7,
                    reference: state.alpha_reference & 0xFF,
                },
            };
            (primitive, vertices, state.surface, state.texture, dma, pipeline)
        };
        let Some(target) = Self::render_target(&surface).filter(|_| !vertices.is_empty()) else {
            self.stats.skipped_primitives += 1;
            return;
        };

        struct Sink<'a>(&'a dyn Nv2aMemory);
        impl crate::PixelSink for Sink<'_> {
            fn write_pixel(&self, physical: u32, color: u32) {
                self.0.write_dword(physical, color);
            }

            fn read_pixel(&self, physical: u32) -> u32 {
                self.0.read_dword(physical).unwrap_or(0)
            }
        }
        let sink = Sink(memory);

        // The bound texture, when this engine can address and read it.
        let bound = texture.enabled.then(|| Self::bind_texture(memory, &texture, texture_dma));
        match bound {
            Some(Some(_)) => {
                self.stats.textured_primitives += 1;
                self.last_texture = Some((texture, texture_dma));
            }
            Some(None) => {
                // The primitive is textured with a format this engine
                // cannot decode. Drawing it flat would paint its vertex
                // color over the frame — and over the render targets a
                // title composites from — so it is left undrawn and
                // counted instead.
                self.stats.unsupported_textures += 1;
                return;
            }
            None => {}
        }
        let bound = bound.flatten();
        let sampler = bound.as_ref().map(|texture| texture as &dyn crate::TextureSource);

        // Primitive assembly follows the hardware's numbering; triangles,
        // strips, fans, and quads are what a title draws with.
        let count = vertices.len();
        let triangles: Vec<[usize; 3]> = match primitive {
            PRIMITIVE_TRIANGLES => (0..count / 3).map(|i| [i * 3, i * 3 + 1, i * 3 + 2]).collect(),
            PRIMITIVE_TRIANGLE_STRIP => (0..count.saturating_sub(2))
                .map(|i| if i % 2 == 0 { [i, i + 1, i + 2] } else { [i + 1, i, i + 2] })
                .collect(),
            PRIMITIVE_TRIANGLE_FAN => (1..count.saturating_sub(1)).map(|i| [0, i, i + 1]).collect(),
            PRIMITIVE_QUADS => (0..count / 4)
                .flat_map(|q| [[q * 4, q * 4 + 1, q * 4 + 2], [q * 4, q * 4 + 2, q * 4 + 3]])
                .collect(),
            _ => {
                self.stats.skipped_primitives += 1;
                return;
            }
        };
        if self.last_draw_target != target.base {
            // Only a display-sized surface is a frame; the smaller ones a
            // title renders into are effect and composite textures, and
            // capturing one of those would report an intermediate step as
            // the picture.
            let (pitch, width, height) = self.last_draw_geometry;
            if pitch != 0 && width >= PRESENTABLE_WIDTH {
                let finished = (self.last_draw_target, pitch, width, height);
                self.previous_draw = Some(finished);
                self.completed_frame = Some(finished);
            }
            self.last_draw_target = target.base;
            tracing::debug!(
                base = format_args!("{:#010x}", target.base),
                pitch = target.pitch,
                width = target.width,
                height = target.height,
                "nv2a draws into a new target"
            );
        }
        self.last_draw_geometry = (target.pitch, target.width, target.height);
        for [a, b, c] in triangles {
            let written = crate::fill_triangle(
                &sink,
                &target,
                [vertices[a], vertices[b], vertices[c]],
                sampler,
                pipeline,
            );
            self.stats.triangles += 1;
            self.stats.shaded_pixels += written;
            *self.pixels_by_target.entry(target.base).or_insert(0) += written;
            if written > 0 {
                self.operations += 1;
                let operation = self.operations;
                let history = self.surface_history.entry(target.base).or_default();
                history.drawn = operation;
                history.geometry = (target.pitch, target.width, target.height);
                history.blended += written;
            }
        }
    }

    /// Resolves the bound texture into something the rasterizer can read.
    ///
    /// Returns `None` when the format is one this engine does not decode
    /// yet — the compressed ones — so the caller can count the gap instead
    /// of sampling nonsense.
    fn bind_texture<'a>(
        memory: &'a dyn Nv2aMemory,
        texture: &TextureState,
        dma: Option<DmaObject>,
    ) -> Option<MemoryTexture<'a>> {
        let (width, height) = texture.extent();
        if width == 0 || height == 0 {
            return None;
        }
        let layout = match texture.color_format() {
            TEXTURE_FORMAT_LINEAR_A8R8G8B8 | TEXTURE_FORMAT_LINEAR_X8R8G8B8 => {
                TextureLayout::Linear
            }
            TEXTURE_FORMAT_SWIZZLED_A8R8G8B8 => TextureLayout::Swizzled,
            TEXTURE_FORMAT_DXT1 => TextureLayout::Dxt1,
            TEXTURE_FORMAT_DXT3 => TextureLayout::Dxt3,
            TEXTURE_FORMAT_DXT5 => TextureLayout::Dxt5,
            _ => return None,
        };
        if layout == TextureLayout::Linear && texture.pitch == 0 {
            return None;
        }
        // A texture may not be larger than the hardware can address, and
        // its samples may not leave the object it was bound to.
        if width > MAX_TEXTURE_DIMENSION || height > MAX_TEXTURE_DIMENSION {
            return None;
        }
        let object = dma?;
        let base = object.base.wrapping_add(texture.offset);
        Some(MemoryTexture {
            memory,
            base,
            pitch: texture.pitch,
            width,
            height,
            layout,
            has_alpha: texture.color_format() != TEXTURE_FORMAT_LINEAR_X8R8G8B8,
            end: base.saturating_add(object.limit).saturating_add(1),
        })
    }

    /// The rasterizer's view of the current color surface.
    fn render_target(surface: &SurfaceState) -> Option<crate::RenderTarget> {
        let color = surface.color_dma?;
        if surface.color_pitch == 0 || surface.clip_width == 0 || surface.clip_height == 0 {
            return None;
        }
        // The clip is two 16-bit guest fields: on its own it permits a
        // 65535-by-65535 fill, which is weeks of work for one primitive.
        // A surface is no larger than the hardware draws into, and no
        // larger than the object holding it.
        let rows = color.limit / surface.color_pitch.max(1);
        let width = surface.clip_width.min(MAX_SURFACE_DIMENSION).min(surface.color_pitch / 4);
        let height = surface.clip_height.min(MAX_SURFACE_DIMENSION).min(rows.max(1));
        if width == 0 || height == 0 {
            return None;
        }
        Some(crate::RenderTarget {
            base: color.base.wrapping_add(surface.color_offset),
            pitch: surface.color_pitch,
            clip_x: surface.clip_x,
            clip_y: surface.clip_y,
            width,
            height,
            // The depth surface shares the color surface's context object.
            zeta: (surface.zeta_pitch != 0 && surface.zeta_offset != 0)
                .then(|| color.base.wrapping_add(surface.zeta_offset)),
            zeta_pitch: surface.zeta_pitch,
        })
    }

    /// The dword count one attribute occupies in a vertex.
    fn attribute_dwords(format: AttributeFormat) -> u32 {
        match format.kind {
            _ if format.size == 0 => 0,
            // Packed colors arrive as one dword whatever their size says.
            ATTRIBUTE_TYPE_D3DCOLOR | ATTRIBUTE_TYPE_UBYTE_RGBA => 1,
            _ => format.size,
        }
    }

    /// Places one vertex on the surface.
    ///
    /// With a program the transform stage has already applied the viewport,
    /// so only the perspective divide remains; without one the fixed
    /// pipeline transforms by the composite matrix and the viewport scale
    /// and offset map the result onto the surface.
    fn place_vertex(
        state: &VertexState,
        transform: &TransformState,
        program: &[[u32; 4]],
        attributes: &[[f32; 4]],
    ) -> Option<crate::ScreenVertex> {
        let color = |value: f32| ((value.clamp(0.0, 1.0) * 255.0) as u32) & 0xFF;
        let (position, diffuse, texcoord) = if transform.programmed && !program.is_empty() {
            let result = crate::execute(program, &transform.constants, attributes)?;
            (result.position, result.diffuse, result.texcoord0)
        } else {
            // The fixed pipeline transforms by the composite matrix, which
            // a title builds with the viewport already folded in: its third
            // row carries the depth scale, and the viewport register is
            // left holding a sub-pixel offset. Each clip component is that
            // row dotted with the position.
            let source = attributes[ATTRIBUTE_POSITION];
            let matrix = &state.composite;
            let mut clip = [0.0_f32; 4];
            for (row, lane) in clip.iter_mut().enumerate() {
                *lane = (0..4).map(|column| source[column] * matrix[row * 4 + column]).sum();
            }
            let texcoord = attributes[ATTRIBUTE_TEXCOORD0];
            (clip, attributes[ATTRIBUTE_DIFFUSE], [texcoord[0], texcoord[1]])
        };

        let [x, y, z, w] = position;
        if w == 0.0 || !w.is_finite() {
            return None;
        }
        let inverse_w = 1.0 / w;
        // Both paths reach the surface already scaled: a program applies
        // the viewport from constants, and the fixed pipeline has it folded
        // into the matrix, leaving only the register's sub-pixel offset.
        let (x, y) = if transform.programmed && !program.is_empty() {
            (x / w, y / w)
        } else {
            let [offset_x, offset_y, ..] = state.viewport_offset;
            (x / w + offset_x, y / w + offset_y)
        };
        Some(crate::ScreenVertex {
            x,
            y,
            color: (color(diffuse[3]) << 24)
                | (color(diffuse[0]) << 16)
                | (color(diffuse[1]) << 8)
                | color(diffuse[2]),
            u: texcoord[0],
            v: texcoord[1],
            // Depth reaches the surface already divided, as the transform
            // that produced it folded in the depth scale.
            z: z * inverse_w,
            inverse_w,
        })
    }

    /// Builds screen-space vertices from arrays in memory.
    ///
    /// Each index names one vertex; every enabled attribute is fetched from
    /// its own array at that index's stride, which is how a title draws
    /// geometry it uploaded once and reuses.
    fn assemble_arrays(
        memory: &dyn Nv2aMemory,
        state: &VertexState,
        transform: &TransformState,
        vertex_dma: &[Option<DmaObject>; 2],
    ) -> Vec<crate::ScreenVertex> {
        let program = transform.program_from_start();
        let mut attributes = default_attributes();
        let mut vertices = Vec::with_capacity(state.elements.len());
        for index in &state.elements {
            for (attribute, format) in state.formats.iter().enumerate() {
                if format.size == 0 {
                    continue;
                }
                let stride = state.array_strides[attribute];
                let raw_offset = state.array_offsets[attribute];
                let context = usize::from(raw_offset & VERTEX_ARRAY_CONTEXT_B != 0);
                let Some(object) = vertex_dma[context].or(vertex_dma[0]) else {
                    continue;
                };
                let start = object.base.wrapping_add(raw_offset & !VERTEX_ARRAY_CONTEXT_B);
                let base = start.wrapping_add(index.wrapping_mul(stride));
                // An index the title never meant to send would otherwise
                // read another object's memory as geometry.
                if base < start || base.saturating_sub(start) > object.limit {
                    continue;
                }
                let register = &mut attributes[attribute];
                match format.kind {
                    ATTRIBUTE_TYPE_D3DCOLOR | ATTRIBUTE_TYPE_UBYTE_RGBA => {
                        let packed = memory.read_dword(base).unwrap_or(0);
                        let channel = |shift: u32| f32::from((packed >> shift) as u8) / 255.0;
                        *register = [channel(16), channel(8), channel(0), channel(24)];
                    }
                    _ => {
                        *register = [0.0, 0.0, 0.0, 1.0];
                        for (component, lane) in
                            register.iter_mut().enumerate().take(format.size.min(4) as usize)
                        {
                            *lane = memory
                                .read_dword(base.wrapping_add(component as u32 * 4))
                                .map_or(0.0, f32::from_bits);
                        }
                    }
                }
            }
            if let Some(vertex) = Self::place_vertex(state, transform, program, &attributes) {
                vertices.push(vertex);
            }
        }
        vertices
    }

    /// Unpacks one vertex's dwords into the attribute registers a program
    /// reads.
    fn load_attributes(state: &VertexState, chunk: &[u32], attributes: &mut [[f32; 4]]) {
        let mut offset = 0_usize;
        for (index, format) in state.formats.iter().enumerate() {
            let dwords = Self::attribute_dwords(*format) as usize;
            if dwords == 0 {
                continue;
            }
            let Some(register) = attributes.get_mut(index) else {
                offset += dwords;
                continue;
            };
            match format.kind {
                ATTRIBUTE_TYPE_D3DCOLOR | ATTRIBUTE_TYPE_UBYTE_RGBA => {
                    let packed = chunk.get(offset).copied().unwrap_or(0);
                    let channel = |shift: u32| f32::from((packed >> shift) as u8) / 255.0;
                    // A packed color reads as red, green, blue, alpha.
                    *register = [channel(16), channel(8), channel(0), channel(24)];
                }
                _ => {
                    // Components the vertex does not carry read as the
                    // hardware's defaults: zero, with a one in `w`.
                    *register = [0.0, 0.0, 0.0, 1.0];
                    for (component, lane) in register.iter_mut().enumerate().take(dwords.min(4)) {
                        *lane =
                            chunk.get(offset + component).map_or(0.0, |bits| f32::from_bits(*bits));
                    }
                }
            }
            offset += dwords;
        }
    }

    /// Builds screen-space vertices from the inline dword stream.
    ///
    /// The declared attribute layout gives each vertex its stride. Positions
    /// arrive in clip space, and the viewport scale and offset place them on
    /// the surface — the same transform the hardware applies after the
    /// vertex stage.
    fn assemble_inline(
        state: &VertexState,
        transform: &TransformState,
    ) -> Vec<crate::ScreenVertex> {
        let position = state.formats[ATTRIBUTE_POSITION];
        if position.kind != ATTRIBUTE_TYPE_FLOAT || position.size < 2 {
            return Vec::new();
        }
        let stride: u32 = state.formats.iter().copied().map(Self::attribute_dwords).sum();
        if stride == 0 || state.inline.len() < stride as usize {
            return Vec::new();
        }
        let diffuse = state.formats[ATTRIBUTE_DIFFUSE];
        let diffuse_offset: usize = state.formats[..ATTRIBUTE_DIFFUSE]
            .iter()
            .copied()
            .map(Self::attribute_dwords)
            .sum::<u32>() as usize;
        let texcoord = state.formats[ATTRIBUTE_TEXCOORD0];
        let texcoord_offset: usize = state.formats[..ATTRIBUTE_TEXCOORD0]
            .iter()
            .copied()
            .map(Self::attribute_dwords)
            .sum::<u32>() as usize;

        // The declared layout, once per primitive: it is the only record of
        // what a title's vertices actually contain.
        if tracing::enabled!(tracing::Level::TRACE) {
            let formats: Vec<String> = state
                .formats
                .iter()
                .enumerate()
                .filter(|(_, format)| format.size != 0)
                .map(|(index, format)| format!("{index}:t{}s{}", format.kind, format.size))
                .collect();
            tracing::trace!(
                stride,
                formats = formats.join(" "),
                first = format_args!("{:08x?}", &state.inline[..stride.min(16) as usize]),
                scale = format_args!("{:?}", state.viewport_scale),
                offset = format_args!("{:?}", state.viewport_offset),
                "nv2a vertex layout"
            );
        }
        // With a program uploaded, the transform stage places the geometry
        // and the viewport maps its clip-space result onto the surface.
        // Without one, only positions already in pixels can be placed
        // truthfully: pre-transformed geometry arrives far outside any clip
        // volume, which is how it is recognized, and anything else is left
        // undrawn rather than painted at object-space coordinates.
        let programmed = transform.programmed && !transform.program.is_empty();
        if !programmed {
            let pretransformed = state.inline.chunks_exact(stride as usize).take(3).any(|chunk| {
                let x = f32::from_bits(chunk[0]).abs();
                let y = f32::from_bits(chunk[1]).abs();
                let w =
                    if position.size >= 4 { f32::from_bits(chunk[3]).abs().max(1.0) } else { 1.0 };
                x > w * 2.0 || y > w * 2.0
            });
            if !pretransformed {
                return Vec::new();
            }
        }
        let program = transform.program_from_start();
        let mut vertices = Vec::with_capacity(state.inline.len() / stride as usize);
        // Attribute values as the program reads them, rebuilt per vertex.
        let mut attributes = default_attributes();
        for chunk in state.inline.chunks_exact(stride as usize) {
            if programmed {
                Self::load_attributes(state, chunk, &mut attributes);
                if let Some(vertex) = Self::place_vertex(state, transform, program, &attributes) {
                    vertices.push(vertex);
                }
                continue;
            }
            let color = match diffuse.kind {
                ATTRIBUTE_TYPE_D3DCOLOR if diffuse.size != 0 => {
                    chunk.get(diffuse_offset).copied().unwrap_or(0xFFFF_FFFF)
                }
                ATTRIBUTE_TYPE_UBYTE_RGBA if diffuse.size != 0 => {
                    // RGBA bytes in memory order become ARGB.
                    let packed = chunk.get(diffuse_offset).copied().unwrap_or(0xFFFF_FFFF);
                    packed.rotate_right(8)
                }
                ATTRIBUTE_TYPE_FLOAT if diffuse.size >= 3 => {
                    let component = |index: usize| {
                        let value = chunk
                            .get(diffuse_offset + index)
                            .map_or(1.0, |bits| f32::from_bits(*bits));
                        ((value.clamp(0.0, 1.0) * 255.0) as u32) & 0xFF
                    };
                    0xFF00_0000 | (component(0) << 16) | (component(1) << 8) | component(2)
                }
                // No color attribute: white keeps the geometry visible.
                _ => 0xFFFF_FFFF,
            };
            let (u, v) = if texcoord.kind == ATTRIBUTE_TYPE_FLOAT && texcoord.size >= 2 {
                (
                    chunk.get(texcoord_offset).map_or(0.0, |bits| f32::from_bits(*bits)),
                    chunk.get(texcoord_offset + 1).map_or(0.0, |bits| f32::from_bits(*bits)),
                )
            } else {
                (0.0, 0.0)
            };
            vertices.push(crate::ScreenVertex {
                x: f32::from_bits(chunk[0]),
                y: f32::from_bits(chunk[1]),
                color,
                u,
                v,
                // Pre-transformed geometry arrives with its depth already
                // in surface units and no perspective left to correct.
                z: chunk.get(2).map_or(0.0, |bits| f32::from_bits(*bits)),
                inverse_w: 1.0,
            });
        }
        vertices
    }

    /// Fills the clear rectangle of the depth surface with its clear value.
    fn clear_zeta_surface(memory: &dyn Nv2aMemory, surface: &SurfaceState) {
        let Some(color) = surface.color_dma else {
            return;
        };
        if surface.zeta_pitch == 0 || surface.zeta_offset == 0 {
            return;
        }
        let Some((left, right, top, bottom)) = surface.clear_bounds(surface.zeta_pitch) else {
            return;
        };
        let base = color.base.wrapping_add(surface.zeta_offset);
        for row in top..bottom {
            let scanline = base.wrapping_add(row.wrapping_mul(surface.zeta_pitch));
            let start = scanline.wrapping_add(left * 4);
            // The depth surface shares the colour object, so it shares its
            // limit: a clear may not run past the end of it.
            if start.wrapping_sub(color.base) > color.limit {
                break;
            }
            memory.fill_dwords(start, surface.clear_zstencil, right - left);
        }
    }

    /// Fills the clear rectangle of the color surface with its clear value.
    ///
    /// The rectangle is intersected with the surface clip, exactly as the
    /// hardware does, and the surface is assumed to be a 32-bit format:
    /// those are the SDTV modes a title scans out, and the pitch says as
    /// much.
    fn clear_color_surface(&mut self, memory: &dyn Nv2aMemory, surface: &SurfaceState) {
        let Some(color) = surface.color_dma else {
            return;
        };
        if surface.color_pitch == 0 {
            return;
        }
        let Some((left, right, top, bottom)) = surface.clear_bounds(surface.color_pitch) else {
            return;
        };

        tracing::debug!(
            left,
            right,
            top,
            bottom,
            pitch = surface.color_pitch,
            base = format_args!("{:#010x}", color.base),
            offset = format_args!("{:#010x}", surface.color_offset),
            limit = format_args!("{:#010x}", color.limit),
            "nv2a color clear"
        );
        let base = color.base.wrapping_add(surface.color_offset);
        let mut written = 0_u64;
        for row in top..bottom {
            let scanline = base.wrapping_add(row.wrapping_mul(surface.color_pitch));
            let start = scanline.wrapping_add(left * 4);
            if start.wrapping_sub(base) > color.limit {
                break;
            }
            written += u64::from(memory.fill_dwords(start, surface.clear_color, right - left));
        }
        if written > 0 {
            self.stats.surface_clears += 1;
            self.stats.cleared_pixels += written;
            self.operations += 1;
            let operation = self.operations;
            let history = self.surface_history.entry(base).or_default();
            history.cleared = operation;
            history.blended = 0;
        }
    }

    /// Decodes the most recently sampled texture into 8-bit RGBA pixels.
    ///
    /// Returns its width, height, and `width * height * 4` bytes.
    #[must_use]
    pub fn dump_last_texture(&self, memory: &dyn Nv2aMemory) -> Option<(u32, u32, Vec<u8>)> {
        use crate::TextureSource;

        let (state, dma) = self.last_texture?;
        let texture = Self::bind_texture(memory, &state, dma)?;
        let (width, height) = (texture.width(), texture.height());
        let bytes = u64::from(width) * u64::from(height) * 4;
        let mut pixels = Vec::with_capacity(usize::try_from(bytes).unwrap_or(0));
        for y in 0..height {
            for x in 0..width {
                let texel = texture.texel(x, y);
                pixels.extend_from_slice(&[
                    ((texel >> 16) & 0xFF) as u8,
                    ((texel >> 8) & 0xFF) as u8,
                    (texel & 0xFF) as u8,
                    ((texel >> 24) & 0xFF) as u8,
                ]);
            }
        }
        tracing::debug!(
            width,
            height,
            format = format_args!("{:#04x}", state.color_format()),
            offset = format_args!("{:#010x}", state.offset),
            base = format_args!("{:#010x}", dma.map_or(0, |object| object.base)),
            "nv2a texture dumped"
        );
        Some((width, height, pixels))
    }

    /// The color surfaces that received the most pixels, most first.
    #[must_use]
    pub fn busiest_targets(&self, limit: usize) -> Vec<(u32, u64)> {
        let mut entries: Vec<(u32, u64)> =
            self.pixels_by_target.iter().map(|(base, count)| (*base, *count)).collect();
        entries.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        entries.truncate(limit);
        entries
    }

    /// The finished frame's color surface, as `(base, pitch, width,
    /// height)`.
    ///
    /// That is the surface drawn into *before* the current one: a title
    /// double-buffers, so while it draws into one the other is on screen.
    /// Before a second surface appears, the only drawn one is reported.
    #[must_use]
    pub fn presented_target(&self) -> Option<(u32, u32, u32, u32)> {
        self.presentable_targets().into_iter().next()
    }

    /// The transform program as uploaded, from its start slot.
    ///
    /// Returns the instruction words so a caller can disassemble them; the
    /// program is the only description of how a title places its geometry.
    #[must_use]
    pub fn transform_program(&self, channel: u32) -> Vec<[u32; 4]> {
        let Some(state) = self.channels.get(&channel) else {
            return Vec::new();
        };
        let start = state.transform.program_start as usize;
        state.transform.program.get(start..).map(<[[u32; 4]]>::to_vec).unwrap_or_default()
    }

    /// The transform constants as uploaded.
    #[must_use]
    pub fn transform_constants(&self, channel: u32) -> Vec<[f32; 4]> {
        self.channels
            .get(&channel)
            .map(|state| state.transform.constants.clone())
            .unwrap_or_default()
    }

    /// Takes the frame finished since this was last called.
    ///
    /// A frame is finished when the title starts drawing into a different
    /// display-sized surface: whatever it had been drawing is now whole.
    pub fn take_completed_frame(&mut self) -> Option<(u32, u32, u32, u32)> {
        self.completed_frame.take()
    }

    /// Every display-sized surface the engine has drawn into, the most
    /// recently drawn first.
    ///
    /// Which one is on screen depends on where the title is in its buffer
    /// rotation, and a caller that can read the surfaces settles that by
    /// looking at them.
    #[must_use]
    pub fn presentable_targets(&self) -> Vec<(u32, u32, u32, u32)> {
        let mut candidates: Vec<(&u32, &SurfaceHistory)> = self
            .surface_history
            .iter()
            .filter(|(_, history)| history.geometry.1 >= PRESENTABLE_WIDTH && history.drawn > 0)
            .collect();
        candidates.sort_by_key(|(_, history)| std::cmp::Reverse(history.drawn));
        candidates
            .into_iter()
            .map(|(base, history)| {
                let (pitch, width, height) = history.geometry;
                (*base, pitch, width, height)
            })
            .collect()
    }

    /// The aggregate statistics.
    #[must_use]
    pub fn stats(&self) -> &PusherStats {
        &self.stats
    }

    /// The distinct methods seen, most frequent first, for diagnostics.
    #[must_use]
    pub fn top_methods(&self, limit: usize) -> Vec<(u32, u16, u64)> {
        let mut entries: Vec<(u32, u16, u64)> = self
            .method_counts
            .iter()
            .map(|((handle, method), count)| (*handle, *method, *count))
            .collect();
        entries.sort_by_key(|(_, _, count)| std::cmp::Reverse(*count));
        entries.truncate(limit);
        entries
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A flat physical memory fake.
    struct FakeMemory(Mutex<Vec<u8>>);

    impl FakeMemory {
        fn new(bytes: usize) -> Self {
            Self(Mutex::new(vec![0; bytes]))
        }

        fn write_words(&self, physical: u32, words: &[u32]) {
            let mut memory = self.0.lock().expect("lock");
            for (index, word) in words.iter().enumerate() {
                let at = physical as usize + index * 4;
                memory[at..at + 4].copy_from_slice(&word.to_le_bytes());
            }
        }

        fn read_word(&self, physical: u32) -> u32 {
            let memory = self.0.lock().expect("lock");
            let at = physical as usize;
            u32::from_le_bytes(memory[at..at + 4].try_into().expect("aligned"))
        }
    }

    impl Nv2aMemory for FakeMemory {
        fn read_dword(&self, physical: u32) -> Option<u32> {
            let memory = self.0.lock().expect("lock");
            let at = physical as usize;
            let slice = memory.get(at..at + 4)?;
            Some(u32::from_le_bytes(slice.try_into().expect("aligned")))
        }

        fn write_dword(&self, physical: u32, value: u32) -> bool {
            let mut memory = self.0.lock().expect("lock");
            let at = physical as usize;
            let Some(slice) = memory.get_mut(at..at + 4) else {
                return false;
            };
            slice.copy_from_slice(&value.to_le_bytes());
            true
        }
    }

    /// An increasing-method header.
    fn header(count: u32, subchannel: u32, method: u16) -> u32 {
        (count << 18) | (subchannel << 13) | u32::from(method)
    }

    #[test]
    fn methods_walk_and_count() {
        let memory = FakeMemory::new(0x1_0000);
        // Two increasing methods at 0x100 (subchannel 3), then a jump to
        // 0x2000 holding one non-increasing method, then end at put.
        memory.write_words(0x1000, &[header(2, 3, 0x0100), 0xAAAA, 0xBBBB, 0x2000_0000 | 0x2000]);
        memory.write_words(0x2000, &[0x4000_0000 | header(2, 0, 0x0200), 0x1, 0x2]);
        let mut engine = PushbufferEngine::default();

        let end = engine.submit(&memory, 0, 0x8000, 0, 0x1000, 0x200C);

        assert_eq!(end, 0x200C, "the walk lands exactly on put");
        assert_eq!(engine.stats().method_dwords, 4);
        // The non-increasing pair stays on one method.
        assert_eq!(engine.top_methods(8).iter().find(|(_, m, _)| *m == 0x200).unwrap().2, 2);
        // The increasing pair spans 0x100 and 0x104.
        assert!(engine.top_methods(8).iter().any(|(_, m, c)| *m == 0x100 && *c == 1));
        assert!(engine.top_methods(8).iter().any(|(_, m, c)| *m == 0x104 && *c == 1));
    }

    #[test]
    fn semaphore_release_writes_memory_through_ramht() {
        let memory = FakeMemory::new(0x2_0000);
        let pramin = 0x8000_u32;
        // RAMHT at instance offset 0 (raw 0 → offset 0, 4096 bytes → 512
        // entries, 9 index bits). Handle 0xBEEF hashes with channel 0.
        let handle = 0xBEEF_u32;
        let hash = ramht_hash(handle, 9, 0) % 512;
        // Entry: handle, context → instance 0x100 (bytes 0x1000).
        memory.write_words(pramin + hash * 8, &[handle, 0x100]);
        // The DMA object at PRAMIN + 0x1000: class 0x3D, limit, frame 0x9000.
        memory.write_words(pramin + 0x1000, &[0x0000_003D, 0xFFF, 0x9000]);

        // Pushbuffer: bind the semaphore context, offset 0x10, release 42.
        memory.write_words(
            0x1000,
            &[
                header(1, 0, METHOD_SET_CONTEXT_DMA_SEMAPHORE),
                handle,
                header(1, 0, METHOD_SET_SEMAPHORE_OFFSET),
                0x10,
                header(1, 0, METHOD_BACK_END_WRITE_SEMAPHORE_RELEASE),
                42,
            ],
        );
        let mut engine = PushbufferEngine::default();

        let end = engine.submit(&memory, 0, pramin, 0, 0x1000, 0x1018);

        assert_eq!(end, 0x1018);
        assert_eq!(engine.stats().semaphore_releases, 1);
        assert_eq!(memory.read_word(0x9000 + 0x10), 42, "the release landed in memory");
    }

    #[test]
    fn a_color_clear_fills_the_surface_rectangle() {
        let memory = FakeMemory::new(0x2_0000);
        let pramin = 0x8000_u32;
        // A color context-DMA object reachable through RAMHT, framing the
        // surface at physical 0xC000.
        let handle = 0x1234_u32;
        let hash = ramht_hash(handle, 9, 0) % 512;
        memory.write_words(pramin + hash * 8, &[handle, 0x100]);
        memory.write_words(pramin + 0x1000, &[0x0000_003D, 0xFFFF, 0xC000]);

        // A 4x4 surface with a 16-byte pitch, cleared over its middle 2x2.
        memory.write_words(
            0x1000,
            &[
                header(1, 0, METHOD_SET_CONTEXT_DMA_COLOR),
                handle,
                header(1, 0, METHOD_SET_SURFACE_CLIP_HORIZONTAL),
                4 << 16,
                header(1, 0, METHOD_SET_SURFACE_CLIP_VERTICAL),
                4 << 16,
                header(1, 0, METHOD_SET_SURFACE_PITCH),
                16,
                header(1, 0, METHOD_SET_SURFACE_COLOR_OFFSET),
                0,
                header(1, 0, METHOD_SET_COLOR_CLEAR_VALUE),
                0xFF20_3040,
                header(1, 0, METHOD_SET_CLEAR_RECT_HORIZONTAL),
                1 | (3 << 16),
                header(1, 0, METHOD_SET_CLEAR_RECT_VERTICAL),
                1 | (3 << 16),
                header(1, 0, METHOD_CLEAR_SURFACE),
                0xF0,
            ],
        );
        let mut engine = PushbufferEngine::default();

        engine.submit(&memory, 0, pramin, 0, 0x1000, 0x1000 + 18 * 4);

        assert_eq!(engine.stats().surface_clears, 1);
        assert_eq!(engine.stats().cleared_pixels, 4, "a 2x2 rectangle");
        // Rows 1 and 2, columns 1 and 2 carry the clear value.
        assert_eq!(memory.read_word(0xC000 + 16 + 4), 0xFF20_3040);
        assert_eq!(memory.read_word(0xC000 + 16 + 8), 0xFF20_3040);
        assert_eq!(memory.read_word(0xC000 + 32 + 4), 0xFF20_3040);
        // Everything outside the rectangle is untouched.
        assert_eq!(memory.read_word(0xC000), 0, "the first pixel is outside");
        assert_eq!(memory.read_word(0xC000 + 16), 0, "column 0 is outside");
        assert_eq!(memory.read_word(0xC000 + 48 + 12), 0, "the last row is outside");
    }

    #[test]
    fn a_clear_without_a_color_context_writes_nothing() {
        let memory = FakeMemory::new(0x1_0000);
        memory.write_words(
            0x1000,
            &[
                header(1, 0, METHOD_SET_SURFACE_PITCH),
                16,
                header(1, 0, METHOD_SET_CLEAR_RECT_HORIZONTAL),
                4 << 16,
                header(1, 0, METHOD_CLEAR_SURFACE),
                0xF0,
            ],
        );
        let mut engine = PushbufferEngine::default();

        engine.submit(&memory, 0, 0x8000, 0, 0x1000, 0x1018);

        assert_eq!(engine.stats().surface_clears, 0, "no surface, no write");
    }

    #[test]
    fn a_depth_only_clear_leaves_the_color_surface_alone() {
        let memory = FakeMemory::new(0x2_0000);
        let pramin = 0x8000_u32;
        let handle = 0x1234_u32;
        let hash = ramht_hash(handle, 9, 0) % 512;
        memory.write_words(pramin + hash * 8, &[handle, 0x100]);
        memory.write_words(pramin + 0x1000, &[0x0000_003D, 0xFFFF, 0xC000]);
        memory.write_words(
            0x1000,
            &[
                header(1, 0, METHOD_SET_CONTEXT_DMA_COLOR),
                handle,
                header(1, 0, METHOD_SET_SURFACE_CLIP_HORIZONTAL),
                4 << 16,
                header(1, 0, METHOD_SET_SURFACE_CLIP_VERTICAL),
                4 << 16,
                header(1, 0, METHOD_SET_SURFACE_PITCH),
                16,
                header(1, 0, METHOD_SET_CLEAR_RECT_HORIZONTAL),
                4 << 16,
                header(1, 0, METHOD_SET_CLEAR_RECT_VERTICAL),
                4 << 16,
                header(1, 0, METHOD_SET_COLOR_CLEAR_VALUE),
                0xFFFF_FFFF,
                // Depth and stencil only.
                header(1, 0, METHOD_CLEAR_SURFACE),
                0x03,
            ],
        );
        let mut engine = PushbufferEngine::default();

        engine.submit(&memory, 0, pramin, 0, 0x1000, 0x1000 + 16 * 4);

        assert_eq!(engine.stats().surface_clears, 0);
        assert_eq!(memory.read_word(0xC000), 0, "the color surface is untouched");
    }

    #[test]
    fn an_inline_triangle_shades_the_surface() {
        let memory = FakeMemory::new(0x2_0000);
        let pramin = 0x8000_u32;
        let handle = 0x1234_u32;
        let hash = ramht_hash(handle, 9, 0) % 512;
        memory.write_words(pramin + hash * 8, &[handle, 0x100]);
        memory.write_words(pramin + 0x1000, &[0x0000_003D, 0xFFFF, 0xC000]);

        // An 8x8 surface, a viewport mapping clip space straight through,
        // one float3 position attribute, and a triangle over the top-left.
        let float3 = 0x32_u32; // type 2 (float), size 3.
        let mut words = vec![
            header(1, 0, METHOD_SET_CONTEXT_DMA_COLOR),
            handle,
            header(1, 0, METHOD_SET_SURFACE_CLIP_HORIZONTAL),
            8 << 16,
            header(1, 0, METHOD_SET_SURFACE_CLIP_VERTICAL),
            8 << 16,
            header(1, 0, METHOD_SET_SURFACE_PITCH),
            32,
            header(1, 0, METHOD_SET_SURFACE_COLOR_OFFSET),
            0,
            header(1, 0, METHOD_SET_VIEWPORT_SCALE),
            1.0_f32.to_bits(),
            header(1, 0, METHOD_SET_VIEWPORT_SCALE + 4),
            1.0_f32.to_bits(),
            header(1, 0, METHOD_SET_VIEWPORT_OFFSET),
            0.0_f32.to_bits(),
            header(1, 0, METHOD_SET_VIEWPORT_OFFSET + 4),
            0.0_f32.to_bits(),
            header(1, 0, METHOD_SET_VERTEX_DATA_ARRAY_FORMAT),
            float3,
            header(1, 0, METHOD_SET_BEGIN_END),
            PRIMITIVE_TRIANGLES,
        ];
        // Three vertices of three floats each, through one non-increasing
        // INLINE_ARRAY run.
        let vertices: [f32; 9] = [0.0, 0.0, 0.0, 8.0, 0.0, 0.0, 0.0, 8.0, 0.0];
        words.push((9 << 18) | (1 << 30) | u32::from(METHOD_INLINE_ARRAY));
        words.extend(vertices.iter().map(|value| value.to_bits()));
        words.push(header(1, 0, METHOD_SET_BEGIN_END));
        words.push(0);
        memory.write_words(0x1000, &words);
        let mut engine = PushbufferEngine::default();

        engine.submit(&memory, 0, pramin, 0, 0x1000, 0x1000 + words.len() as u32 * 4);

        assert_eq!(engine.stats().triangles, 1, "one triangle was assembled");
        assert!(engine.stats().shaded_pixels > 20, "it covered its half of the surface");
        assert_eq!(engine.stats().skipped_primitives, 0);
        // The top-left pixel is inside the triangle; the bottom-right is not.
        assert_eq!(memory.read_word(0xC000), 0xFFFF_FFFF, "geometry with no color draws white");
        assert_eq!(memory.read_word(0xC000 + 7 * 32 + 7 * 4), 0, "the far corner stays clear");
    }

    #[test]
    fn a_primitive_without_a_position_attribute_is_skipped() {
        let memory = FakeMemory::new(0x2_0000);
        let pramin = 0x8000_u32;
        let handle = 0x1234_u32;
        let hash = ramht_hash(handle, 9, 0) % 512;
        memory.write_words(pramin + hash * 8, &[handle, 0x100]);
        memory.write_words(pramin + 0x1000, &[0x0000_003D, 0xFFFF, 0xC000]);
        let words = [
            header(1, 0, METHOD_SET_CONTEXT_DMA_COLOR),
            handle,
            header(1, 0, METHOD_SET_SURFACE_PITCH),
            32,
            header(1, 0, METHOD_SET_BEGIN_END),
            PRIMITIVE_TRIANGLES,
            header(1, 0, METHOD_INLINE_ARRAY),
            0,
            header(1, 0, METHOD_SET_BEGIN_END),
            0,
        ];
        memory.write_words(0x1000, &words);
        let mut engine = PushbufferEngine::default();

        engine.submit(&memory, 0, pramin, 0, 0x1000, 0x1000 + words.len() as u32 * 4);

        assert_eq!(engine.stats().triangles, 0);
        assert_eq!(engine.stats().skipped_primitives, 1, "the gap is counted, not hidden");
    }

    /// Runs one pushbuffer of words against a fresh engine, returning it.
    ///
    /// Every test below submits guest data no title would send. None may
    /// panic, hang, or read outside the objects the stream names.
    fn submit_words(words: &[u32]) -> (PushbufferEngine, FakeMemory) {
        let memory = FakeMemory::new(0x2_0000);
        memory.write_words(0x1000, words);
        let mut engine = PushbufferEngine::default();
        engine.submit(&memory, 0, 0x8000, 0, 0x1000, 0x1000 + words.len() as u32 * 4);
        (engine, memory)
    }

    #[test]
    fn a_transform_start_past_the_program_draws_nothing() {
        let (engine, _) = submit_words(&[
            header(1, 0, METHOD_SET_TRANSFORM_EXECUTION_MODE),
            TRANSFORM_MODE_PROGRAM,
            header(1, 0, METHOD_SET_TRANSFORM_PROGRAM_START),
            0xFFFF_FFFF,
            header(1, 0, METHOD_SET_VERTEX_DATA_ARRAY_FORMAT),
            0x32,
            header(1, 0, METHOD_SET_BEGIN_END),
            PRIMITIVE_TRIANGLES,
            header(1, 0, METHOD_ARRAY_ELEMENT32),
            0,
            header(1, 0, METHOD_SET_BEGIN_END),
            0,
        ]);
        assert_eq!(engine.stats().triangles, 0, "an empty program places no geometry");
    }

    #[test]
    fn a_transform_load_pointer_at_the_top_of_the_range_does_not_overflow() {
        let (engine, _) = submit_words(&[
            header(1, 0, METHOD_SET_TRANSFORM_PROGRAM_LOAD),
            0x4000_0000,
            header(1, 0, METHOD_SET_TRANSFORM_PROGRAM),
            0xDEAD_BEEF,
            header(1, 0, METHOD_SET_TRANSFORM_CONSTANT_LOAD),
            0x3FFF_FFFF,
            header(4, 0, METHOD_SET_TRANSFORM_CONSTANT),
            0,
            0,
            0,
            0,
        ]);
        // Reaching here at all is the assertion: these words used to panic
        // on the multiply and on the increment past the end of the range.
        assert_eq!(engine.stats().aborted, 0);
    }

    #[test]
    fn a_vast_surface_clip_does_not_fill_forever() {
        let handle = 0x1234_u32;
        let hash = ramht_hash(handle, 9, 0) % 512;
        let memory = FakeMemory::new(0x2_0000);
        memory.write_words(0x8000 + hash * 8, &[handle, 0x100]);
        memory.write_words(0x8000 + 0x1000, &[0x0000_003D, 0xFFFF, 0xC000]);
        let words = [
            header(1, 0, METHOD_SET_CONTEXT_DMA_COLOR),
            handle,
            // Sixty-five thousand pixels on a side, as the two 16-bit
            // halves of the clip permit.
            header(1, 0, METHOD_SET_SURFACE_CLIP_HORIZONTAL),
            0xFFFF_0000,
            header(1, 0, METHOD_SET_SURFACE_CLIP_VERTICAL),
            0xFFFF_0000,
            header(1, 0, METHOD_SET_SURFACE_PITCH),
            32,
            header(1, 0, METHOD_SET_CLEAR_RECT_HORIZONTAL),
            0xFFFF_0000,
            header(1, 0, METHOD_SET_CLEAR_RECT_VERTICAL),
            0xFFFF_0000,
            header(1, 0, METHOD_SET_COLOR_CLEAR_VALUE),
            0xFFFF_FFFF,
            header(1, 0, METHOD_CLEAR_SURFACE),
            CLEAR_COLOR_MASK,
        ];
        memory.write_words(0x1000, &words);
        let mut engine = PushbufferEngine::default();
        let start = std::time::Instant::now();

        engine.submit(&memory, 0, 0x8000, 0, 0x1000, 0x1000 + words.len() as u32 * 4);

        assert!(
            start.elapsed().as_secs() < 5,
            "the clear is bounded by the surface, not the field"
        );
        assert!(
            engine.stats().cleared_pixels < 1 << 26,
            "cleared {} pixels",
            engine.stats().cleared_pixels
        );
    }

    #[test]
    fn a_circular_pushbuffer_of_long_runs_exhausts_its_budget() {
        let memory = FakeMemory::new(0x2_0000);
        // A two-thousand-count method header, its arguments, then a jump
        // back to the header: the pusher must charge every dword it reads.
        let mut words = vec![(2047 << 18) | u32::from(0x0100_u16)];
        words.extend(std::iter::repeat_n(0, 2047));
        words.push(0x2000_0000 | 0x1000);
        memory.write_words(0x1000, &words);
        let mut engine = PushbufferEngine::default();
        let start = std::time::Instant::now();

        engine.submit(&memory, 0, 0x8000, 0, 0x1000, 0x1000 + words.len() as u32 * 4);

        assert!(start.elapsed().as_secs() < 10, "the budget bounds the walk");
        assert!(
            engine.stats().method_dwords <= u64::from(MAX_DWORDS_PER_SUBMIT),
            "consumed {} dwords",
            engine.stats().method_dwords
        );
    }

    #[test]
    fn a_texture_larger_than_the_hardware_is_refused() {
        let handle = 0x1234_u32;
        let hash = ramht_hash(handle, 9, 0) % 512;
        let memory = FakeMemory::new(0x2_0000);
        memory.write_words(0x8000 + hash * 8, &[handle, 0x100]);
        memory.write_words(0x8000 + 0x1000, &[0x0000_003D, 0xFFFF, 0xC000]);
        let words = [
            header(1, 0, METHOD_SET_CONTEXT_DMA_A),
            handle,
            header(1, 0, METHOD_SET_TEXTURE_OFFSET),
            0,
            // Thirty-two thousand texels on a side.
            header(1, 0, METHOD_SET_TEXTURE_FORMAT),
            0xFF00_1201,
            header(1, 0, METHOD_SET_TEXTURE_CONTROL1),
            4 << 16,
            header(1, 0, METHOD_SET_TEXTURE_CONTROL0),
            TEXTURE_ENABLE,
        ];
        memory.write_words(0x1000, &words);
        let mut engine = PushbufferEngine::default();
        engine.submit(&memory, 0, 0x8000, 0, 0x1000, 0x1000 + words.len() as u32 * 4);

        assert!(
            engine.dump_last_texture(&memory).is_none(),
            "no sampler for an impossible texture"
        );
    }

    #[test]
    fn the_method_census_stops_growing() {
        // A fresh object handle before every method would otherwise key a
        // new census entry each time until memory ran out.
        let mut words = Vec::new();
        for handle in 0..(MAX_CENSUS_ENTRIES as u32 + 64) {
            words.push(header(1, 0, METHOD_SET_OBJECT));
            words.push(handle + 1);
            words.push(header(1, 0, 0x0100));
            words.push(0);
        }
        let (engine, _) = submit_words(&words);
        assert!(
            engine.top_methods(usize::MAX).len() <= MAX_CENSUS_ENTRIES,
            "the census is capped at {MAX_CENSUS_ENTRIES}"
        );
    }

    #[test]
    fn call_and_return_round_trip() {
        let memory = FakeMemory::new(0x1_0000);
        // Call 0x3000, which runs one method and returns; then one more
        // method inline.
        memory.write_words(0x1000, &[0x3000 | 0x2, header(1, 0, 0x0100), 0x77]);
        memory.write_words(0x3000, &[header(1, 0, 0x0400), 0x55, 0x0002_0000]);
        let mut engine = PushbufferEngine::default();

        let end = engine.submit(&memory, 0, 0x8000, 0, 0x1000, 0x100C);

        assert_eq!(end, 0x100C);
        assert_eq!(engine.stats().method_dwords, 2);
    }

    #[test]
    fn a_bad_word_abandons_the_walk() {
        let memory = FakeMemory::new(0x1_0000);
        memory.write_words(0x1000, &[0xDEAD_BEEF]);
        let mut engine = PushbufferEngine::default();

        let end = engine.submit(&memory, 0, 0x8000, 0, 0x1000, 0x1010);

        assert_eq!(end, 0x1000, "the abandonment point is the bad word");
        assert_eq!(engine.stats().aborted, 1);
    }
}
