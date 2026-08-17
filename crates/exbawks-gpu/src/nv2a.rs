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
    /// The clear rectangle's horizontal bounds (left, right).
    clear_x: (u32, u32),
    /// The clear rectangle's vertical bounds (top, bottom).
    clear_y: (u32, u32),
}

/// One vertex attribute's declared layout.
#[derive(Debug, Default, Clone, Copy)]
struct AttributeFormat {
    /// The component type code (`2` is 32-bit float).
    kind: u32,
    /// The component count; zero disables the attribute.
    size: u32,
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
    /// The viewport's scale, as programmed.
    viewport_scale: [f32; 4],
    /// The viewport's offset, as programmed.
    viewport_offset: [f32; 4],
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
    /// Submissions abandoned mid-walk (bad word or unreadable memory).
    pub aborted: u64,
}

/// One submission's fixed parameters.
struct SubmitContext {
    channel: u32,
    pramin: u32,
    ramht_raw: u32,
}

/// The DMA pusher: walks submitted command ranges and applies effects.
#[derive(Debug, Default)]
pub struct PushbufferEngine {
    channels: HashMap<u32, ChannelState>,
    /// Per-method dword counts, keyed by (subchannel-bound handle, method).
    method_counts: HashMap<(u32, u16), u64>,
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
/// Kelvin `SET_COLOR_CLEAR_VALUE`.
const METHOD_SET_COLOR_CLEAR_VALUE: u16 = 0x1D90;
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
/// The vertex attribute carrying position.
const ATTRIBUTE_POSITION: usize = 0;
/// The vertex attribute carrying the diffuse color.
const ATTRIBUTE_DIFFUSE: usize = 3;
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
        *self.method_counts.entry((bound, method)).or_insert(0) += 1;
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
            }
            METHOD_SET_SURFACE_COLOR_OFFSET => {
                state.surface.color_offset = argument;
            }
            METHOD_SET_COLOR_CLEAR_VALUE => {
                state.surface.clear_color = argument;
            }
            METHOD_SET_CLEAR_RECT_HORIZONTAL => {
                state.surface.clear_x = (argument & 0xFFFF, argument >> 16);
            }
            METHOD_SET_CLEAR_RECT_VERTICAL => {
                state.surface.clear_y = (argument & 0xFFFF, argument >> 16);
            }
            METHOD_CLEAR_SURFACE => {
                if argument & CLEAR_COLOR_MASK != 0 {
                    let surface = self.channels.entry(channel).or_default().surface;
                    self.clear_color_surface(memory, &surface);
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
            }
            METHOD_SET_BEGIN_END => {
                if argument == 0 {
                    self.end_primitive(memory, channel);
                } else {
                    state.vertex.primitive = Some(argument);
                    state.vertex.inline.clear();
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
        let (primitive, vertices, surface) = {
            let state = self.channels.entry(channel).or_default();
            let Some(primitive) = state.vertex.primitive.take() else {
                return;
            };
            let vertices = Self::assemble_inline(&state.vertex);
            state.vertex.inline.clear();
            (primitive, vertices, state.surface)
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
        }
        let sink = Sink(memory);

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
            let (pitch, width, height) = self.last_draw_geometry;
            if pitch != 0 {
                self.previous_draw = Some((self.last_draw_target, pitch, width, height));
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
            let written =
                crate::fill_triangle(&sink, &target, [vertices[a], vertices[b], vertices[c]]);
            self.stats.triangles += 1;
            self.stats.shaded_pixels += written;
        }
    }

    /// The rasterizer's view of the current color surface.
    fn render_target(surface: &SurfaceState) -> Option<crate::RenderTarget> {
        let color = surface.color_dma?;
        if surface.color_pitch == 0 || surface.clip_width == 0 || surface.clip_height == 0 {
            return None;
        }
        Some(crate::RenderTarget {
            base: color.base.wrapping_add(surface.color_offset),
            pitch: surface.color_pitch,
            clip_x: surface.clip_x,
            clip_y: surface.clip_y,
            width: surface.clip_width,
            height: surface.clip_height,
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

    /// Builds screen-space vertices from the inline dword stream.
    ///
    /// The declared attribute layout gives each vertex its stride. Positions
    /// arrive in clip space, and the viewport scale and offset place them on
    /// the surface — the same transform the hardware applies after the
    /// vertex stage.
    fn assemble_inline(state: &VertexState) -> Vec<crate::ScreenVertex> {
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
        let [scale_x, scale_y, ..] = state.viewport_scale;
        let [offset_x, offset_y, ..] = state.viewport_offset;
        // A vertex program owns the transform, and this engine has none, so
        // the only positions it can place truthfully are the ones already
        // in pixels: a title's pre-transformed geometry (`D3DFVF_XYZRHW`)
        // arrives far outside any clip volume, which is exactly how it is
        // recognized. Everything else keeps the viewport transform, which
        // is right once a transform stage exists to feed it.
        let pretransformed = state.inline.chunks_exact(stride as usize).take(3).any(|chunk| {
            let x = f32::from_bits(chunk[0]).abs();
            let y = f32::from_bits(chunk[1]).abs();
            let w = if position.size >= 4 { f32::from_bits(chunk[3]).abs().max(1.0) } else { 1.0 };
            x > w * 2.0 || y > w * 2.0
        });
        let mut vertices = Vec::with_capacity(state.inline.len() / stride as usize);
        for chunk in state.inline.chunks_exact(stride as usize) {
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
            let x = f32::from_bits(chunk[0]);
            let y = f32::from_bits(chunk[1]);
            let (x, y) = if pretransformed {
                (x, y)
            } else {
                (x.mul_add(scale_x, offset_x), y.mul_add(scale_y, offset_y))
            };
            vertices.push(crate::ScreenVertex { x, y, color });
        }
        vertices
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
        let left = surface.clear_x.0.max(surface.clip_x);
        let right = surface.clear_x.1.min(surface.clip_x.saturating_add(surface.clip_width));
        let top = surface.clear_y.0.max(surface.clip_y);
        let bottom = surface.clear_y.1.min(surface.clip_y.saturating_add(surface.clip_height));
        if left >= right || top >= bottom {
            return;
        }

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
        }
    }

    /// The finished frame's color surface, as `(base, pitch, width,
    /// height)`.
    ///
    /// That is the surface drawn into *before* the current one: a title
    /// double-buffers, so while it draws into one the other is on screen.
    /// Before a second surface appears, the only drawn one is reported.
    #[must_use]
    pub fn presented_target(&self) -> Option<(u32, u32, u32, u32)> {
        self.previous_draw.or_else(|| {
            let (pitch, width, height) = self.last_draw_geometry;
            (pitch != 0).then_some((self.last_draw_target, pitch, width, height))
        })
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
