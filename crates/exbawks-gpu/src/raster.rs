//! Triangle rasterization into a linear 32-bit color surface.
//!
//! The pushbuffer engine assembles primitives; this turns them into pixels.
//! It is a half-space rasterizer: a triangle's three edge functions are
//! evaluated per pixel over the primitive's bounding box, which keeps fill
//! rules consistent for both windings and needs no clipping beyond the
//! surface rectangle.
//!
//! Colors interpolate barycentrically. Depth, texturing, and blending are
//! not modeled yet; a drawn pixel replaces what was there.

/// One assembled vertex in screen space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenVertex {
    /// The horizontal position in pixels.
    pub x: f32,
    /// The vertical position in pixels.
    pub y: f32,
    /// The color as 8-bit ARGB.
    pub color: u32,
    /// The four texture coordinate sets, in units of each texture's
    /// extent. A combiner stage reads whichever unit it names, and a title
    /// gives each its own coordinates.
    pub texcoords: [[f32; 2]; 4],
    /// The depth, in the units the depth surface stores.
    pub z: f32,
    /// The reciprocal of the clip-space `w`, for perspective-correct
    /// interpolation. One for geometry that arrives already projected.
    pub inverse_w: f32,
}

/// A bound texture the rasterizer can sample.
pub trait TextureSource: Sync {
    /// The texel at integer coordinates, as 8-bit ARGB.
    ///
    /// Coordinates are already brought inside the texture's extent.
    fn texel(&self, x: u32, y: u32) -> u32;

    /// The texture's width in texels.
    fn width(&self) -> u32;

    /// The texture's height in texels.
    fn height(&self) -> u32;

    /// How coordinates outside the texture are brought back inside, on
    /// each axis. Clamping is the default because it is what a sampler
    /// with nothing programmed does.
    fn addressing(&self) -> [u32; 2] {
        [ADDRESS_CLAMP_TO_EDGE; 2]
    }

    /// Whether samples are blended between neighbouring texels.
    fn filtered(&self) -> bool {
        false
    }

    /// How many mip levels the texture carries.
    fn levels(&self) -> u32 {
        1
    }

    /// The texel at integer coordinates within one mip level.
    fn texel_in(&self, _level: u32, x: u32, y: u32) -> u32 {
        self.texel(x, y)
    }

    /// That level's width in texels.
    fn width_in(&self, _level: u32) -> u32 {
        self.width()
    }

    /// That level's height in texels.
    fn height_in(&self, _level: u32) -> u32 {
        self.height()
    }
}

/// Repeat the texture, dropping the whole part of the coordinate.
pub const ADDRESS_WRAP: u32 = 1;
/// Repeat it, reversing direction on every other tile.
pub const ADDRESS_MIRROR: u32 = 2;
/// Hold the edge texel for anything outside.
pub const ADDRESS_CLAMP_TO_EDGE: u32 = 3;

/// Brings one texel coordinate inside `extent` by the programmed mode.
///
/// A title that asks to repeat a texture and gets clamping instead sees
/// the edge texel smeared across everything past the first tile, which
/// reads as a broad flat band rather than as a pattern.
fn address_texel(mode: u32, texel: i64, extent: u32) -> u32 {
    let span = i64::from(extent);
    if span <= 0 {
        return 0;
    }
    match mode {
        ADDRESS_WRAP => texel.rem_euclid(span) as u32,
        ADDRESS_MIRROR => {
            let period = texel.rem_euclid(span * 2);
            let mirrored = if period < span { period } else { span * 2 - 1 - period };
            mirrored as u32
        }
        _ => texel.clamp(0, span - 1) as u32,
    }
}

/// Which mip level a triangle should sample from one unit.
///
/// The level comes from how many texels the triangle covers per pixel: a
/// surface squeezed into a quarter of its texture's area reads one level
/// down. Taking it once per triangle rather than per pixel is an
/// approximation — a steeply perspective triangle wants a level that
/// varies across it — but it is the difference between reading a
/// minified texture as noise and reading it as an image.
fn mip_level(
    texture: &dyn TextureSource,
    vertices: [ScreenVertex; 3],
    unit: usize,
    area: f32,
) -> u32 {
    let levels = texture.levels();
    if levels <= 1 || area <= 0.0 {
        return 0;
    }
    let [a, b, c] = vertices;
    let width = texture.width() as f32;
    let height = texture.height() as f32;
    let (first_u, first_v) = (a.texcoords[unit][0] * width, a.texcoords[unit][1] * height);
    let edge_one =
        [b.texcoords[unit][0] * width - first_u, b.texcoords[unit][1] * height - first_v];
    let edge_two =
        [c.texcoords[unit][0] * width - first_u, c.texcoords[unit][1] * height - first_v];
    // Both areas are doubled, so the doubling cancels in the ratio.
    let covered = edge_one[0].mul_add(edge_two[1], -(edge_two[0] * edge_one[1])).abs();
    if covered <= 0.0 || !covered.is_finite() {
        return 0;
    }
    let level = 0.5 * (covered / area).log2();
    if !level.is_finite() || level <= 0.0 {
        return 0;
    }
    (level.round() as u32).min(levels - 1)
}

/// What a bound texture unit contributes, gathered once per triangle.
///
/// A sample otherwise asks the texture its extent, its addressing, and
/// whether it filters, through a trait object, for every corner of every
/// pixel — seven such calls a sample, none of which change while a
/// triangle is being drawn.
struct Sampler<'a> {
    texture: &'a dyn TextureSource,
    level: u32,
    width: u32,
    height: u32,
    mode_u: u32,
    mode_v: u32,
    filtered: bool,
}

impl Sampler<'_> {
    /// The colour at one coordinate, in texels.
    fn sample(&self, u: f32, v: f32) -> u32 {
        if self.filtered { self.filtered_at(u, v) } else { self.nearest_at(u, v) }
    }

    /// The texel a coordinate lands on, with no blending.
    fn nearest_at(&self, u: f32, v: f32) -> u32 {
        // The floor, not a truncation: a negative coordinate must land in
        // the tile below zero, not back in the first one.
        self.texture.texel_in(
            self.level,
            address_texel(self.mode_u, u.floor() as i64, self.width),
            address_texel(self.mode_v, v.floor() as i64, self.height),
        )
    }

    /// The four texels around a coordinate, blended by how near it is to
    /// each.
    fn filtered_at(&self, u: f32, v: f32) -> u32 {
        // Texel centres sit at the half, so the blend is between the
        // texels either side rather than biased to one of them.
        let (u, v) = (u - 0.5, v - 0.5);
        let (left, top) = (u.floor(), v.floor());
        let (fraction_u, fraction_v) = (u - left, v - top);

        // A guest hands over its coordinates as raw float bits, so the
        // scaled value can saturate the cast; reaching for the
        // neighbouring texel must not then overflow.
        let column = |offset: i64| {
            address_texel(self.mode_u, (left as i64).saturating_add(offset), self.width)
        };
        let row = |offset: i64| {
            address_texel(self.mode_v, (top as i64).saturating_add(offset), self.height)
        };
        let (x0, x1, y0, y1) = (column(0), column(1), row(0), row(1));
        let corners = [
            self.texture.texel_in(self.level, x0, y0),
            self.texture.texel_in(self.level, x1, y0),
            self.texture.texel_in(self.level, x0, y1),
            self.texture.texel_in(self.level, x1, y1),
        ];
        let weights = [
            (1.0 - fraction_u) * (1.0 - fraction_v),
            fraction_u * (1.0 - fraction_v),
            (1.0 - fraction_u) * fraction_v,
            fraction_u * fraction_v,
        ];

        let mut blended = 0_u32;
        for shift in [0, 8, 16, 24] {
            let channel: f32 = (0..4)
                .map(|corner| ((corners[corner] >> shift) & 0xFF) as f32 * weights[corner])
                .sum();
            blended |= ((channel + 0.5).clamp(0.0, 255.0) as u32) << shift;
        }
        blended
    }
}

/// What an unbound texture unit reads, which is white.
const WHITE_CHANNELS: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// One over the largest channel value, so unpacking multiplies.
const CHANNEL_SCALE: f32 = 1.0 / 255.0;

/// Splits an 8-bit ARGB color into the combiner's float channels.
fn unpack_channels(color: u32) -> [f32; 4] {
    [
        ((color >> 16) & 0xFF) as f32 * CHANNEL_SCALE,
        ((color >> 8) & 0xFF) as f32 * CHANNEL_SCALE,
        (color & 0xFF) as f32 * CHANNEL_SCALE,
        ((color >> 24) & 0xFF) as f32 * CHANNEL_SCALE,
    ]
}

/// Multiplies two ARGB colors channel by channel.
fn modulate(left: u32, right: u32) -> u32 {
    let mut out = 0_u32;
    for shift in [0, 8, 16, 24] {
        let product = ((left >> shift) & 0xFF) * ((right >> shift) & 0xFF);
        out |= (((product + 127) / 255) & 0xFF) << shift;
    }
    out
}

/// The color surface a primitive lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderTarget {
    /// The surface's base physical address.
    pub base: u32,
    /// The distance between scanlines in bytes.
    pub pitch: u32,
    /// The drawable rectangle's left edge in pixels.
    pub clip_x: u32,
    /// The drawable rectangle's top edge in pixels.
    pub clip_y: u32,
    /// The drawable rectangle's width in pixels.
    pub width: u32,
    /// The drawable rectangle's height in pixels.
    pub height: u32,
    /// The depth surface's base physical address, when one is bound.
    pub zeta: Option<u32>,
    /// The distance between depth scanlines in bytes.
    pub zeta_pitch: u32,
}

impl RenderTarget {
    /// The physical address of one depth value, when a surface is bound.
    fn depth_address(&self, x: u32, y: u32) -> Option<u32> {
        let base = self.zeta?;
        base.checked_add(y.checked_mul(self.zeta_pitch)?)?.checked_add(x.checked_mul(4)?)
    }
}

impl RenderTarget {
    /// The physical address of one pixel, when it lies inside the clip.
    fn pixel_address(&self, x: u32, y: u32) -> Option<u32> {
        if x < self.clip_x
            || y < self.clip_y
            || x >= self.clip_x.saturating_add(self.width)
            || y >= self.clip_y.saturating_add(self.height)
        {
            return None;
        }
        self.base.checked_add(y.checked_mul(self.pitch)?)?.checked_add(x.checked_mul(4)?)
    }
}

/// A pixel sink: the emulator reads and writes guest physical memory.
pub trait PixelSink: Sync {
    /// Writes one 32-bit pixel at a physical address.
    fn write_pixel(&self, physical: u32, color: u32);

    /// Reads the pixel already at a physical address, for blending.
    fn read_pixel(&self, physical: u32) -> u32;
}

/// How a fragment's depth is compared against the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DepthState {
    /// Whether a fragment must pass the comparison to be drawn.
    pub test: bool,
    /// Whether a passing fragment updates the depth surface.
    pub write: bool,
    /// The comparison, in the hardware's numbering: never, less, equal,
    /// less-or-equal, greater, not-equal, greater-or-equal, always.
    pub function: u32,
}

impl DepthState {
    /// Whether a fragment at `value` passes against `stored`.
    fn passes(self, value: u32, stored: u32) -> bool {
        match self.function & 0x7 {
            0 => false,
            1 => value < stored,
            2 => value == stored,
            3 => value <= stored,
            4 => value > stored,
            5 => value != stored,
            6 => value >= stored,
            _ => true,
        }
    }
}

/// How a drawn pixel combines with what is already on the surface.
///
/// The factors are the hardware's own codes, which a title programs per
/// draw: it leans on `ONE` for the passes that add light and on
/// `ONE_MINUS_SRC_ALPHA` for the ones that lay art over a background, and
/// treating the first like the second darkens exactly what should glow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlendState {
    /// Whether the drawn pixel is combined at all.
    pub enabled: bool,
    /// The factor the drawn pixel is weighted by.
    pub source: u32,
    /// The factor the destination is weighted by.
    pub destination: u32,
}

impl Default for BlendState {
    fn default() -> Self {
        // Blending off: the drawn pixel replaces what is there.
        Self { enabled: false, source: BLEND_ONE, destination: BLEND_ZERO }
    }
}

/// The blend factor codes this engine reads.
pub const BLEND_ZERO: u32 = 0x0000;
/// One.
pub const BLEND_ONE: u32 = 0x0001;
const BLEND_SRC_COLOR: u32 = 0x0300;
const BLEND_ONE_MINUS_SRC_COLOR: u32 = 0x0301;
const BLEND_SRC_ALPHA: u32 = 0x0302;
const BLEND_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
const BLEND_DST_ALPHA: u32 = 0x0304;
const BLEND_ONE_MINUS_DST_ALPHA: u32 = 0x0305;
const BLEND_DST_COLOR: u32 = 0x0306;
const BLEND_ONE_MINUS_DST_COLOR: u32 = 0x0307;
const BLEND_SRC_ALPHA_SATURATE: u32 = 0x0308;

/// One blend factor, as a weight in `0..=255` for one channel.
fn blend_factor(code: u32, source: u32, destination: u32, shift: u32) -> u32 {
    let channel = |color: u32| (color >> shift) & 0xFF;
    let source_alpha = (source >> 24) & 0xFF;
    let destination_alpha = (destination >> 24) & 0xFF;
    match code {
        BLEND_ZERO => 0,
        BLEND_SRC_COLOR => channel(source),
        BLEND_ONE_MINUS_SRC_COLOR => 255 - channel(source),
        BLEND_SRC_ALPHA => source_alpha,
        BLEND_ONE_MINUS_SRC_ALPHA => 255 - source_alpha,
        BLEND_DST_ALPHA => destination_alpha,
        BLEND_ONE_MINUS_DST_ALPHA => 255 - destination_alpha,
        BLEND_DST_COLOR => channel(destination),
        BLEND_ONE_MINUS_DST_COLOR => 255 - channel(destination),
        BLEND_SRC_ALPHA_SATURATE if shift != 24 => source_alpha.min(255 - destination_alpha),
        // `ONE`, and anything this engine does not model, leaves the term
        // as it is rather than dropping it.
        _ => 255,
    }
}

/// Combines a source and destination color by the programmed factors.
fn combine(state: BlendState, source: u32, destination: u32) -> u32 {
    let mut out = 0;
    for shift in [0, 8, 16, 24] {
        let from = (source >> shift) & 0xFF;
        let onto = (destination >> shift) & 0xFF;
        let source_weight = blend_factor(state.source, source, destination, shift);
        let destination_weight = blend_factor(state.destination, source, destination, shift);
        let value = (from * source_weight + onto * destination_weight + 127) / 255;
        out |= (value.min(255) & 0xFF) << shift;
    }
    out
}

/// Whether to take the nearest texel rather than blending four.
///
/// Blending is what a title asks for and costs about a third of the time
/// a run spends drawing. Turning it off trades that fidelity for a frame
/// rate someone can play at, so it is a choice a person makes rather than
/// something decided for them.
///
/// Which faces are discarded before they are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CullState {
    /// Whether facing is tested at all.
    pub enabled: bool,
    /// The face discarded: `0x0404` front, `0x0405` back, `0x0408` both.
    pub face: u32,
    /// The winding that counts as front: `0x0900` clockwise on screen.
    pub front_face: u32,
}

/// The comparison a fragment's alpha must pass to be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AlphaTest {
    /// Whether the comparison runs.
    pub enabled: bool,
    /// The comparison, in the hardware's numbering.
    pub function: u32,
    /// The value compared against, in `0..=255`.
    pub reference: u32,
}

impl AlphaTest {
    /// Whether a fragment's alpha passes.
    fn passes(self, alpha: u32) -> bool {
        if !self.enabled {
            return true;
        }
        match self.function & 0x7 {
            0 => false,
            1 => alpha < self.reference,
            2 => alpha == self.reference,
            3 => alpha <= self.reference,
            4 => alpha > self.reference,
            5 => alpha != self.reference,
            6 => alpha >= self.reference,
            _ => true,
        }
    }
}

/// Everything a primitive is drawn under besides its geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PipelineState {
    /// Whether texture samples take the nearest texel rather than blending
    /// four of them, trading the filtering a title asked for against the
    /// time it costs.
    pub unfiltered: bool,
    /// How a drawn pixel combines with the surface.
    pub blend: BlendState,
    /// How a fragment is compared against the depth surface.
    pub depth: DepthState,
    /// Which faces are discarded.
    pub cull: CullState,
    /// The comparison a fragment's alpha must pass.
    pub alpha: AlphaTest,
}

/// The cull-face code discarding front faces.
const CULL_FACE_FRONT: u32 = 0x0404;
/// The cull-face code discarding back faces.
const CULL_FACE_BACK: u32 = 0x0405;
/// The cull-face code discarding both.
const CULL_FACE_BOTH: u32 = 0x0408;
/// The front-face code naming clockwise winding.
const FRONT_FACE_CLOCKWISE: u32 = 0x0900;

/// The signed area of the triangle formed by three points, doubled.
fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}

/// Interpolates three ARGB colors by barycentric weights.
fn interpolate_color(colors: [u32; 3], weights: [f32; 3]) -> u32 {
    let mut out = 0_u32;
    for shift in [0, 8, 16, 24] {
        let channel = weights
            .iter()
            .zip(colors)
            .map(|(weight, color)| weight * f32::from(((color >> shift) & 0xFF) as u8))
            .sum::<f32>();
        // Round rather than truncate: weights that sum to just under one
        // would otherwise darken a solid color by a level.
        out |= (((channel + 0.5).clamp(0.0, 255.0) as u32) & 0xFF) << shift;
    }
    out
}

/// Fills one triangle, returning the number of pixels written.
///
/// With a texture bound, each pixel samples it at the interpolated
/// coordinate (nearest texel) and modulates the result by the interpolated
/// vertex color, which is what a title's default texture stage does.
pub fn fill_triangle(
    sink: &dyn PixelSink,
    target: &RenderTarget,
    vertices: [ScreenVertex; 3],
    textures: [Option<&dyn TextureSource>; 4],
    state: PipelineState,
    combiner: Option<&crate::CombinerState>,
) -> u64 {
    fill_triangle_rows(sink, target, vertices, textures, state, combiner, (0, u32::MAX))
}

/// Draws every triangle of one draw call, spreading the surface's rows
/// across the host's processors, and reports how many pixels were written.
///
/// Every triangle in a draw call shares its textures, its pipeline state,
/// and its combiner program, so the only thing ordering them matters for is
/// what they leave in the surface — and two triangles only interact where
/// they cover the same pixel. Splitting by rows keeps every such pair on one
/// thread and in submission order, so the frame is the frame a single thread
/// would have drawn, whatever the host's processor count. That is what keeps
/// a recorded digest meaningful.
///
/// Small draws are drawn on the calling thread, because handing out a few
/// hundred pixels costs more than doing the work.
pub fn rasterize_draw(
    sink: &dyn PixelSink,
    target: &RenderTarget,
    vertices: &[ScreenVertex],
    triangles: &[[usize; 3]],
    textures: [Option<&dyn TextureSource>; 4],
    state: PipelineState,
    combiner: Option<&crate::CombinerState>,
) -> u64 {
    let one_band = |rows: (u32, u32)| -> u64 {
        let mut written = 0;
        for [a, b, c] in triangles {
            written += fill_triangle_rows(
                sink,
                target,
                [vertices[*a], vertices[*b], vertices[*c]],
                textures,
                state,
                combiner,
                rows,
            );
        }
        written
    };

    // The rows this draw could touch at all, so a draw covering a corner of
    // the screen is split across that corner rather than across the screen.
    let (mut first, mut last) = (u32::MAX, 0_u32);
    for [a, b, c] in triangles {
        for index in [a, b, c] {
            let y = vertices[*index].y;
            if y.is_finite() {
                first = first.min(y.floor().max(0.0) as u32);
                last = last.max(y.ceil().max(0.0) as u32 + 1);
            }
        }
    }
    let first = first.max(target.clip_y);
    let last = last.min(target.clip_y + target.height);
    if first >= last {
        return 0;
    }

    // How much drawing this actually is, from the triangles' own areas
    // rather than from the box around them: a draw of narrow slivers spans
    // as many rows as a draw of full-width bars and is a fraction of the
    // work, and dividing it by its box hands each band an overhead it
    // cannot pay for.
    let height = last - first;
    let covered: u64 = triangles
        .iter()
        .map(|corners| {
            let [a, b, c] = corners.map(|index| vertices[index]);
            let area = edge(a.x, a.y, b.x, b.y, c.x, c.y).abs() * 0.5;
            if area.is_finite() { area as u64 } else { 0 }
        })
        .sum();
    // Bands are given enough pixels to be worth handing to a processor.
    let bands = (covered / PIXELS_PER_BAND) as usize;
    let bands = bands.min(host_parallelism()).min(height as usize).min(MAX_DRAW_BANDS);
    if bands <= 1 || covered < PARALLEL_DRAW_PIXELS {
        return one_band((first, last));
    }

    // Bands are equal in height rather than in work: which rows are
    // expensive is not known until they are drawn, and an uneven split
    // still spends far less than one thread would.
    let rows_each = height.div_ceil(bands as u32);
    let band_of = |y: u32| ((y.saturating_sub(first)) / rows_each) as usize;

    // Each band is given the triangles that reach its rows, rather than the
    // whole draw. A band that walked the entire list would decide, for
    // every triangle, that it belongs to another band — and a draw of many
    // small triangles is mostly that decision, paid once per band. A tall
    // triangle lands in each band it crosses, which is what draws its part
    // of it there.
    let mut binned: Vec<Vec<[usize; 3]>> = vec![Vec::new(); bands];
    for corners in triangles {
        let ys = corners.map(|index| vertices[index].y);
        let (top, bottom) = (ys[0].min(ys[1]).min(ys[2]), ys[0].max(ys[1]).max(ys[2]));
        if !top.is_finite() || !bottom.is_finite() {
            continue;
        }
        let top = (top.floor().max(0.0) as u32).max(first);
        let bottom = (bottom.ceil().max(0.0) as u32 + 1).min(last);
        if top >= bottom {
            continue;
        }
        for band in binned.iter_mut().take(band_of(bottom - 1) + 1).skip(band_of(top)) {
            band.push(*corners);
        }
    }

    // The bands run on a pool that outlives the draw. A title submits a
    // draw every few hundred microseconds, and creating threads for each
    // one costs more than the drawing does — the work is there, but it has
    // to be handed to processors that are already running.
    use rayon::iter::{IndexedParallelIterator, IntoParallelRefIterator, ParallelIterator};
    binned
        .par_iter()
        .enumerate()
        .map(|(band, own)| {
            let start = first + rows_each * band as u32;
            let rows = (start.min(last), (start + rows_each).min(last));
            let mut written = 0;
            for [a, b, c] in own {
                written += fill_triangle_rows(
                    sink,
                    target,
                    [vertices[*a], vertices[*b], vertices[*c]],
                    textures,
                    state,
                    combiner,
                    rows,
                );
            }
            written
        })
        .sum()
}

/// The most bands one draw is divided into. Past this the threads cost more
/// than the rows they are given, and every band walks the whole triangle
/// list to find the part of it that lands in its own rows.
const MAX_DRAW_BANDS: usize = 64;

/// How many pixels a draw must cover before it is worth dividing.
const PARALLEL_DRAW_PIXELS: u64 = 4_096;

/// How many pixels each band is given before another band is worth adding.
/// Below this a band spends more reaching a processor than drawing.
const PIXELS_PER_BAND: u64 = 512;

/// How many processors the host has, asked once.
///
/// A title submits over a hundred thousand draws in a run, and asking the
/// operating system this for each of them is a system call per draw.
fn host_parallelism() -> usize {
    use std::sync::OnceLock;
    static COUNT: OnceLock<usize> = OnceLock::new();
    *COUNT.get_or_init(|| std::thread::available_parallelism().map_or(1, std::num::NonZero::get))
}

/// Fills the part of a triangle lying in `rows`, a half-open range of
/// scanlines, and reports how many pixels it wrote.
///
/// A row's pixels depend on the triangle and on what is already in the
/// surface beneath them, never on another row, so covering a triangle in
/// several row ranges writes exactly what covering it in one would. That is
/// what lets a draw spread its rows across the host's processors and still
/// produce the frame a single thread would have.
pub fn fill_triangle_rows(
    sink: &dyn PixelSink,
    target: &RenderTarget,
    vertices: [ScreenVertex; 3],
    textures: [Option<&dyn TextureSource>; 4],
    state: PipelineState,
    combiner: Option<&crate::CombinerState>,
    rows: (u32, u32),
) -> u64 {
    let (blend, depth) = (state.blend, state.depth);
    let [a, b, c] = vertices;
    let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
    if area == 0.0 || !area.is_finite() {
        return 0;
    }
    // Screen space runs downward, so a positive signed area is a clockwise
    // triangle as the viewer sees it.
    if state.cull.enabled {
        let clockwise = area > 0.0;
        let front = clockwise == (state.cull.front_face == FRONT_FACE_CLOCKWISE);
        let discarded = match state.cull.face {
            CULL_FACE_FRONT => front,
            CULL_FACE_BACK => !front,
            CULL_FACE_BOTH => true,
            _ => false,
        };
        if discarded {
            return 0;
        }
    }
    let (a, b, c) = if area < 0.0 { (a, c, b) } else { (a, b, c) };
    let area = area.abs();

    let left = a.x.min(b.x).min(c.x).floor().max(target.clip_x as f32);
    let right = a.x.max(b.x).max(c.x).ceil().min((target.clip_x + target.width) as f32);
    let top = a.y.min(b.y).min(c.y).floor().max(target.clip_y as f32).max(rows.0 as f32);
    let bottom =
        a.y.max(b.y).max(c.y).ceil().min((target.clip_y + target.height) as f32).min(rows.1 as f32);
    if !(left < right && top < bottom) {
        return 0;
    }

    // Everything about each bound unit that holds for the whole triangle:
    // its level, that level's extent, how it addresses, and whether it
    // blends. Asking the texture per pixel is asking it per corner.
    let samplers: [Option<Sampler<'_>>; 4] = std::array::from_fn(|unit| {
        let texture = textures[unit]?;
        let level = mip_level(texture, [a, b, c], unit, area);
        let [mode_u, mode_v] = texture.addressing();
        Some(Sampler {
            texture,
            level,
            width: texture.width_in(level),
            height: texture.height_in(level),
            mode_u,
            mode_v,
            filtered: texture.filtered() && !state.unfiltered,
        })
    });
    // Whether the combiner has a program at all, decided once rather than
    // per pixel: an unprogrammed one would build its registers, run
    // nothing, and report nothing, for every pixel of every triangle.
    // Whether the combiner has a program at all, decided once rather than
    // per pixel: an unprogrammed one would build its registers, run
    // nothing, and report nothing, for every pixel of every triangle.
    let programmed = combiner.filter(|state| state.active > 0 || state.final_first != 0);

    let mut written = 0;
    let mut y = top as u32;
    let last_y = bottom as u32;
    let first_x = left as u32;
    let last_x = right as u32;
    while y < last_y {
        let center_y = y as f32 + 0.5;
        for x in first_x..last_x {
            let center_x = x as f32 + 0.5;
            // Each edge is evaluated rather than stepped along the row.
            // Stepping is cheaper in arithmetic and measurably no faster
            // here — the cost is elsewhere — and it accumulates rounding
            // error along a row, which moves pixels at a triangle's edge.
            let weight_a = edge(b.x, b.y, c.x, c.y, center_x, center_y);
            let weight_b = edge(c.x, c.y, a.x, a.y, center_x, center_y);
            let weight_c = edge(a.x, a.y, b.x, b.y, center_x, center_y);
            if weight_a < 0.0 || weight_b < 0.0 || weight_c < 0.0 {
                continue;
            }
            let Some(address) = target.pixel_address(x, y) else {
                continue;
            };
            let weights = [weight_a / area, weight_b / area, weight_c / area];

            // Depth is already divided by `w`, so it interpolates linearly
            // in screen space, as the hardware's does.
            let mut depth_address = None;
            if depth.test || depth.write {
                let value = weights[0] * a.z + weights[1] * b.z + weights[2] * c.z;
                let value = value.clamp(0.0, f32::from(u16::MAX) * 256.0) as u32;
                if let Some(address) = target.depth_address(x, y) {
                    // A `Z24S8` surface keeps its stencil in the low byte.
                    let stored = sink.read_pixel(address) >> 8;
                    if depth.test && !depth.passes(value, stored) {
                        continue;
                    }
                    if depth.write {
                        depth_address = Some((address, value));
                    }
                }
            }

            let diffuse = interpolate_color([a.color, b.color, c.color], weights);
            let mut color = diffuse;
            // Texture coordinates interpolate in the plane of the triangle,
            // not the screen: weight each by the vertex's reciprocal `w`
            // and divide by the interpolated reciprocal, or a textured
            // surface seen at an angle skews.
            let inverse_w =
                weights[0] * a.inverse_w + weights[1] * b.inverse_w + weights[2] * c.inverse_w;
            let scale = if inverse_w == 0.0 { 1.0 } else { 1.0 / inverse_w };
            let mut texels = [None; 4];
            for (unit, sampled) in texels.iter_mut().enumerate() {
                let Some(sampler) = samplers[unit].as_ref() else {
                    continue;
                };
                let coordinate = |axis: usize| {
                    (weights[0] * a.texcoords[unit][axis] * a.inverse_w
                        + weights[1] * b.texcoords[unit][axis] * b.inverse_w
                        + weights[2] * c.texcoords[unit][axis] * c.inverse_w)
                        * scale
                };
                *sampled = Some(sampler.sample(
                    coordinate(0) * sampler.width as f32,
                    coordinate(1) * sampler.height as f32,
                ));
            }
            // Without a combiner program the first unit modulates the
            // vertex color, which is what an unprogrammed stage does.
            if let Some(sampled) = texels[0] {
                color = modulate(sampled, color);
            }
            // The combiners, once a title has programmed them, are the
            // pipeline's real color stage: they replace the fixed modulate
            // above, which is only what an unprogrammed stage would do.
            if let Some(combiner) = programmed {
                // An unbound unit reads white, so a stage that modulates
                // by one leaves its other input alone. Unpacking that
                // white for every unit a title never bound is four
                // conversions a pixel spent on a constant.
                let mut registers = crate::CombinerRegisters {
                    diffuse: unpack_channels(diffuse),
                    textures: [WHITE_CHANNELS; 4],
                    ..crate::CombinerRegisters::default()
                };
                for (unit, sampled) in texels.iter().enumerate() {
                    if let Some(texel) = sampled {
                        registers.textures[unit] = unpack_channels(*texel);
                    }
                }
                if let Some(computed) = crate::evaluate_combiner(combiner, &registers) {
                    color = computed;
                }
            }
            if !state.alpha.passes((color >> 24) & 0xFF) {
                continue;
            }
            if blend.enabled {
                let alpha = (color >> 24) & 0xFF;
                // A transparent pixel under the usual over-blend leaves the
                // surface exactly as it was, and its color channels are
                // black; skipping it saves a read and a write. The additive
                // passes a title uses for light have no such shortcut.
                let transparent_is_a_no_op = alpha == 0
                    && blend.source == BLEND_SRC_ALPHA
                    && blend.destination == BLEND_ONE_MINUS_SRC_ALPHA;
                if transparent_is_a_no_op {
                    continue;
                }
                color = combine(blend, color, sink.read_pixel(address));
            }
            sink.write_pixel(address, color);
            if let Some((address, value)) = depth_address {
                let stencil = sink.read_pixel(address) & 0xFF;
                sink.write_pixel(address, (value << 8) | stencil);
            }
            written += 1;
        }
        y += 1;
    }
    written
}

#[cfg(test)]
mod tests {
    /// A sampler over a whole texture, for the tests below.
    fn sampler_over(texture: &dyn TextureSource, level: u32, filtered: bool) -> Sampler<'_> {
        let [mode_u, mode_v] = texture.addressing();
        Sampler {
            texture,
            level,
            width: texture.width_in(level),
            height: texture.height_in(level),
            mode_u,
            mode_v,
            filtered,
        }
    }

    /// A filtered sample at the base level.
    fn filtered_sample(texture: &dyn TextureSource, u: f32, v: f32) -> u32 {
        sampler_over(texture, 0, true).filtered_at(u, v)
    }

    /// A filtered sample at one level.
    fn filtered_sample_at(texture: &dyn TextureSource, level: u32, u: f32, v: f32) -> u32 {
        sampler_over(texture, level, true).filtered_at(u, v)
    }

    /// The nearest texel at one level.
    fn nearest_sample(texture: &dyn TextureSource, level: u32, u: f32, v: f32) -> u32 {
        sampler_over(texture, level, false).nearest_at(u, v)
    }

    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Canvas(Mutex<Vec<(u32, u32)>>);

    impl PixelSink for Canvas {
        fn write_pixel(&self, physical: u32, color: u32) {
            self.0.lock().expect("lock").push((physical, color));
        }

        fn read_pixel(&self, physical: u32) -> u32 {
            self.0
                .lock()
                .expect("lock")
                .iter()
                .rev()
                .find(|(address, _)| *address == physical)
                .map_or(0xFF00_0000, |(_, color)| *color)
        }
    }

    fn target() -> RenderTarget {
        RenderTarget {
            base: 0x1000,
            pitch: 40,
            clip_x: 0,
            clip_y: 0,
            width: 10,
            height: 10,
            zeta: None,
            zeta_pitch: 0,
        }
    }

    /// The over-blend a title uses for transparent art.
    fn over_blend() -> PipelineState {
        PipelineState {
            blend: BlendState { enabled: true, source: 0x0302, destination: 0x0303 },
            ..PipelineState::default()
        }
    }

    fn vertex(x: f32, y: f32, color: u32) -> ScreenVertex {
        ScreenVertex { x, y, color, texcoords: [[0.0, 0.0]; 4], z: 0.0, inverse_w: 1.0 }
    }

    /// A two-by-two texture whose texels are their own coordinates.
    struct Checker;

    impl TextureSource for Checker {
        fn texel(&self, x: u32, y: u32) -> u32 {
            match (x, y) {
                (0, 0) => 0xFFFF_0000,
                (1, 0) => 0xFF00_FF00,
                (0, 1) => 0xFF00_00FF,
                _ => 0xFFFF_FFFF,
            }
        }

        fn width(&self) -> u32 {
            2
        }

        fn height(&self) -> u32 {
            2
        }
    }

    /// A surface addressed by pixel, so two runs can be compared.
    #[derive(Default)]
    struct Surface(Mutex<std::collections::BTreeMap<u32, u32>>);

    impl PixelSink for Surface {
        fn write_pixel(&self, physical: u32, color: u32) {
            self.0.lock().expect("lock").insert(physical, color);
        }

        fn read_pixel(&self, physical: u32) -> u32 {
            self.0.lock().expect("lock").get(&physical).copied().unwrap_or(0xFF00_0000)
        }
    }

    #[test]
    fn splitting_the_rows_draws_the_same_surface() {
        // The whole of drawing on several processors rests on this: a
        // triangle covered in row ranges leaves exactly what covering it in
        // one pass leaves, blending included, whatever order the ranges are
        // taken in. If this ever stops holding, a recorded frame stops
        // meaning anything.
        let target = RenderTarget {
            base: 0x1000,
            pitch: 64,
            clip_x: 0,
            clip_y: 0,
            width: 16,
            height: 16,
            zeta: None,
            zeta_pitch: 0,
        };
        // Overlapping triangles, so the blend order matters.
        let triangles = [
            [
                vertex(0.0, 0.0, 0xC0FF_0000),
                vertex(15.0, 2.0, 0xC000_FF00),
                vertex(3.0, 15.0, 0xC000_00FF),
            ],
            [
                vertex(2.0, 1.0, 0x8000_FFFF),
                vertex(14.0, 9.0, 0x80FF_00FF),
                vertex(1.0, 14.0, 0x80FF_FF00),
            ],
        ];

        let whole = Surface::default();
        for corners in triangles {
            fill_triangle(&whole, &target, corners, [None; 4], over_blend(), None);
        }

        // The same drawing, but every triangle covered three rows at a
        // time — which is what a band is.
        let banded = Surface::default();
        for rows in [(0, 3), (3, 6), (6, 9), (9, 12), (12, 16)] {
            for corners in triangles {
                fill_triangle_rows(&banded, &target, corners, [None; 4], over_blend(), None, rows);
            }
        }

        let expected = whole.0.lock().expect("lock").clone();
        let actual = banded.0.lock().expect("lock").clone();
        assert!(!expected.is_empty(), "the triangles cover something");
        assert_eq!(actual, expected, "banded drawing matches a single pass");
    }

    #[test]
    fn a_whole_draw_matches_one_triangle_at_a_time() {
        // And the same for the entry point that spreads the bands, which
        // chooses how many to use from how much the draw covers.
        let target = RenderTarget {
            base: 0x2000,
            pitch: 1024,
            clip_x: 0,
            clip_y: 0,
            width: 256,
            height: 256,
            zeta: None,
            zeta_pitch: 0,
        };
        // Large enough that the draw is worth dividing.
        let corners = [
            vertex(0.0, 0.0, 0xC0FF_0000),
            vertex(255.0, 20.0, 0xC000_FF00),
            vertex(40.0, 255.0, 0xC000_00FF),
            vertex(250.0, 250.0, 0x80FF_00FF),
        ];
        let triangles = [[0_usize, 1, 2], [1, 3, 2]];

        let sequential = Surface::default();
        for indices in triangles {
            let picked = indices.map(|index| corners[index]);
            fill_triangle(&sequential, &target, picked, [None; 4], over_blend(), None);
        }

        let spread = Surface::default();
        let drawn =
            rasterize_draw(&spread, &target, &corners, &triangles, [None; 4], over_blend(), None);

        let expected = sequential.0.lock().expect("lock").clone();
        assert!(drawn > 0 && !expected.is_empty(), "the draw covers something");
        assert_eq!(spread.0.lock().expect("lock").clone(), expected, "same surface");
    }

    #[test]
    fn a_right_triangle_covers_its_half() {
        let canvas = Canvas::default();
        // A triangle over the whole 10x10 target, split corner to corner.
        let written = fill_triangle(
            &canvas,
            &target(),
            [
                vertex(0.0, 0.0, 0xFFFFFFFF),
                vertex(10.0, 0.0, 0xFFFFFFFF),
                vertex(0.0, 10.0, 0xFFFFFFFF),
            ],
            [None; 4],
            PipelineState::default(),
            None,
        );
        // Half of a hundred pixels, within the diagonal's rounding.
        assert!((45..=60).contains(&written), "covered {written} pixels");
    }

    #[test]
    fn winding_does_not_change_coverage() {
        let canvas = Canvas::default();
        let clockwise = fill_triangle(
            &canvas,
            &target(),
            [
                vertex(1.0, 1.0, 0xFF00FF00),
                vertex(8.0, 1.0, 0xFF00FF00),
                vertex(1.0, 8.0, 0xFF00FF00),
            ],
            [None; 4],
            PipelineState::default(),
            None,
        );
        let counter = fill_triangle(
            &canvas,
            &target(),
            [
                vertex(1.0, 1.0, 0xFF00FF00),
                vertex(1.0, 8.0, 0xFF00FF00),
                vertex(8.0, 1.0, 0xFF00FF00),
            ],
            [None; 4],
            PipelineState::default(),
            None,
        );
        assert_eq!(clockwise, counter, "both windings fill the same pixels");
    }

    #[test]
    fn pixels_land_at_the_surface_stride() {
        let canvas = Canvas::default();
        fill_triangle(
            &canvas,
            &target(),
            [
                vertex(0.0, 0.0, 0xFF112233),
                vertex(4.0, 0.0, 0xFF112233),
                vertex(0.0, 4.0, 0xFF112233),
            ],
            [None; 4],
            PipelineState::default(),
            None,
        );
        let written = canvas.0.lock().expect("lock");
        let (first_address, first_color) = written[0];
        assert_eq!(first_address, 0x1000, "the first pixel is the surface origin");
        assert_eq!(first_color, 0xFF112233, "a single-color triangle keeps its color");
        // Row one starts one pitch further along.
        assert!(written.iter().any(|(address, _)| *address == 0x1000 + 40));
    }

    #[test]
    fn a_triangle_outside_the_clip_writes_nothing() {
        let canvas = Canvas::default();
        let written = fill_triangle(
            &canvas,
            &target(),
            [
                vertex(20.0, 20.0, 0xFFFFFFFF),
                vertex(30.0, 20.0, 0xFFFFFFFF),
                vertex(20.0, 30.0, 0xFFFFFFFF),
            ],
            [None; 4],
            PipelineState::default(),
            None,
        );
        assert_eq!(written, 0);
        assert!(canvas.0.lock().expect("lock").is_empty());
    }

    #[test]
    fn a_degenerate_triangle_writes_nothing() {
        let canvas = Canvas::default();
        let written = fill_triangle(
            &canvas,
            &target(),
            [
                vertex(1.0, 1.0, 0xFFFFFFFF),
                vertex(5.0, 1.0, 0xFFFFFFFF),
                vertex(9.0, 1.0, 0xFFFFFFFF),
            ],
            [None; 4],
            PipelineState::default(),
            None,
        );
        assert_eq!(written, 0, "a zero-area triangle covers nothing");
    }

    #[test]
    fn colors_interpolate_across_the_triangle() {
        let canvas = Canvas::default();
        fill_triangle(
            &canvas,
            &target(),
            [
                vertex(0.0, 0.0, 0xFFFF0000),
                vertex(9.0, 0.0, 0xFF00FF00),
                vertex(0.0, 9.0, 0xFF0000FF),
            ],
            [None; 4],
            PipelineState::default(),
            None,
        );
        let written = canvas.0.lock().expect("lock");
        let colors: Vec<u32> = written.iter().map(|(_, color)| *color).collect();
        assert!(colors.len() > 10);
        assert!(
            colors.iter().any(|color| color & 0x00FF_0000 > 0x0080_0000),
            "the red corner stays red"
        );
        assert!(
            colors.iter().any(|color| color & 0x0000_FF00 > 0x0000_8000),
            "the green corner stays green"
        );
        assert!(colors.iter().all(|color| color >> 24 == 0xFF), "alpha carries through");
    }

    /// A two-by-two texture: black, white on the top row; white, black
    /// on the bottom.
    struct Corners;

    impl TextureSource for Corners {
        fn texel(&self, x: u32, y: u32) -> u32 {
            if (x + y).is_multiple_of(2) { 0xFF00_0000 } else { 0xFFFF_FFFF }
        }

        fn width(&self) -> u32 {
            2
        }

        fn height(&self) -> u32 {
            2
        }

        fn filtered(&self) -> bool {
            true
        }
    }

    /// A texture that reports a chain and which level it was read at.
    struct Chain(std::sync::atomic::AtomicU32);

    impl TextureSource for Chain {
        fn texel(&self, x: u32, y: u32) -> u32 {
            self.texel_in(0, x, y)
        }

        fn texel_in(&self, level: u32, _x: u32, _y: u32) -> u32 {
            self.0.store(level, std::sync::atomic::Ordering::Relaxed);
            0xFFFF_FFFF
        }

        fn width(&self) -> u32 {
            64
        }

        fn height(&self) -> u32 {
            64
        }

        fn levels(&self) -> u32 {
            7
        }

        fn width_in(&self, level: u32) -> u32 {
            (64 >> level).max(1)
        }

        fn height_in(&self, level: u32) -> u32 {
            (64 >> level).max(1)
        }
    }

    /// A triangle covering `pixels` on a side, mapped to `texels` of the
    /// texture, as the level selector sees it.
    fn spanning(pixels: f32, texels: f32) -> [ScreenVertex; 3] {
        let corner = |x: f32, y: f32, u: f32, v: f32| ScreenVertex {
            x,
            y,
            color: 0xFFFF_FFFF,
            texcoords: [[u, v]; 4],
            z: 0.0,
            inverse_w: 1.0,
        };
        let fraction = texels / 64.0;
        [
            corner(0.0, 0.0, 0.0, 0.0),
            corner(pixels, 0.0, fraction, 0.0),
            corner(0.0, pixels, 0.0, fraction),
        ]
    }

    #[test]
    fn the_mip_level_follows_how_far_a_texture_is_squeezed() {
        let texture = Chain(std::sync::atomic::AtomicU32::new(0));
        let area = |side: f32| side * side;

        // One texel per pixel reads the level the title authored.
        assert_eq!(mip_level(&texture, spanning(64.0, 64.0), 0, area(64.0)), 0);
        // Four texels per pixel — half the size on each side — reads one
        // level down, which is the level that already holds that average.
        assert_eq!(mip_level(&texture, spanning(32.0, 64.0), 0, area(32.0)), 1);
        assert_eq!(mip_level(&texture, spanning(16.0, 64.0), 0, area(16.0)), 2);
        // A texture magnified rather than minified stays at the base.
        assert_eq!(mip_level(&texture, spanning(128.0, 64.0), 0, area(128.0)), 0);
        // And the chain bounds the choice however far it is squeezed.
        assert_eq!(mip_level(&texture, spanning(0.25, 64.0), 0, area(0.25)), 6);
    }

    #[test]
    fn a_sample_reads_the_level_it_was_given() {
        let texture = Chain(std::sync::atomic::AtomicU32::new(0));
        nearest_sample(&texture, 3, 0.5, 0.5);
        assert_eq!(
            texture.0.load(std::sync::atomic::Ordering::Relaxed),
            3,
            "the nearest sample reads its level"
        );
        filtered_sample_at(&texture, 5, 0.5, 0.5);
        assert_eq!(
            texture.0.load(std::sync::atomic::Ordering::Relaxed),
            5,
            "and so does the filtered one"
        );
    }

    #[test]
    fn a_filtered_sample_survives_an_absurd_coordinate() {
        // A guest supplies texture coordinates as raw float bits, so a
        // vertex can carry one large enough that the floored, scaled value
        // saturates the cast. Reaching for the neighbouring texel then
        // overflows, which panics wherever overflow checks are on.
        for coordinate in [1e20_f32, f32::INFINITY, -f32::INFINITY, f32::MAX, f32::NAN] {
            let _ = filtered_sample(&Corners, coordinate, 0.5);
            let _ = filtered_sample(&Corners, 0.5, coordinate);
            let _ = nearest_sample(&Corners, 0, coordinate, coordinate);
        }
    }

    #[test]
    fn a_filtered_sample_blends_the_texels_around_it() {
        // Dead on a texel centre the blend returns that texel alone.
        assert_eq!(filtered_sample(&Corners, 0.5, 0.5), 0xFF00_0000, "the first texel");
        assert_eq!(filtered_sample(&Corners, 1.5, 0.5), 0xFFFF_FFFF, "and its neighbour");

        // Halfway between them it is the average of the two, which is
        // what turns a mosaic back into a gradient.
        let midpoint = filtered_sample(&Corners, 1.0, 0.5);
        let channel = midpoint & 0xFF;
        assert!((127..=128).contains(&channel), "halfway is halfway: {channel}");

        // The centre of the texture touches all four corners equally, and
        // two of them are white.
        let centre = filtered_sample(&Corners, 1.0, 1.0) & 0xFF;
        assert!((127..=128).contains(&centre), "the middle averages four: {centre}");

        // Alpha is blended like any other channel, not dropped.
        assert_eq!(filtered_sample(&Corners, 0.5, 0.5) >> 24, 0xFF);
    }

    #[test]
    fn addressing_brings_a_coordinate_back_inside_the_texture() {
        // Wrapping repeats, which is what a title asking to tile a texture
        // expects; clamping it instead holds one edge texel across
        // everything past the first tile.
        assert_eq!(address_texel(ADDRESS_WRAP, 0, 4), 0);
        assert_eq!(address_texel(ADDRESS_WRAP, 5, 4), 1, "past the end starts over");
        assert_eq!(address_texel(ADDRESS_WRAP, -1, 4), 3, "and below zero comes from the end");

        // Mirroring reverses on every other tile.
        assert_eq!(address_texel(ADDRESS_MIRROR, 3, 4), 3);
        assert_eq!(address_texel(ADDRESS_MIRROR, 4, 4), 3, "the next tile runs backwards");
        assert_eq!(address_texel(ADDRESS_MIRROR, 7, 4), 0);
        assert_eq!(address_texel(ADDRESS_MIRROR, 8, 4), 0, "and the one after that forwards");

        // Clamping holds the edge, and is what an unprogrammed sampler does.
        assert_eq!(address_texel(ADDRESS_CLAMP_TO_EDGE, 9, 4), 3);
        assert_eq!(address_texel(ADDRESS_CLAMP_TO_EDGE, -9, 4), 0);

        // A texture with no extent cannot be indexed at all.
        assert_eq!(address_texel(ADDRESS_WRAP, 3, 0), 0);
    }

    #[test]
    fn a_bound_texture_colors_the_triangle() {
        let canvas = Canvas::default();
        // A triangle covering the target's top-left, with texture
        // coordinates running across the whole texture.
        let mut vertices = [
            vertex(0.0, 0.0, 0xFFFFFFFF),
            vertex(10.0, 0.0, 0xFFFFFFFF),
            vertex(0.0, 10.0, 0xFFFFFFFF),
        ];
        vertices[1].texcoords[0][0] = 1.0;
        vertices[2].texcoords[0][1] = 1.0;
        let written = fill_triangle(
            &canvas,
            &target(),
            vertices,
            [Some(&Checker), None, None, None],
            PipelineState::default(),
            None,
        );

        assert!(written > 0);
        let pixels = canvas.0.lock().expect("lock");
        let colors: Vec<u32> = pixels.iter().map(|(_, color)| *color).collect();
        assert_eq!(colors[0], 0xFFFF_0000, "the origin samples the first texel");
        assert!(colors.contains(&0xFF00_FF00), "the right edge samples across");
        assert!(colors.contains(&0xFF00_00FF), "the bottom edge samples down");
    }

    #[test]
    fn a_vertex_color_modulates_the_texel() {
        let canvas = Canvas::default();
        // Half-intensity vertex color over the first texel (pure red).
        let vertices = [
            vertex(0.0, 0.0, 0xFF80_8080),
            vertex(4.0, 0.0, 0xFF80_8080),
            vertex(0.0, 4.0, 0xFF80_8080),
        ];
        fill_triangle(
            &canvas,
            &target(),
            vertices,
            [Some(&Checker), None, None, None],
            PipelineState::default(),
            None,
        );

        let pixels = canvas.0.lock().expect("lock");
        let (_, color) = pixels[0];
        assert_eq!(color >> 24, 0xFF, "alpha is one times one");
        let red = (color >> 16) & 0xFF;
        assert!((0x7E..=0x82).contains(&red), "red is halved: {red:#x}");
        assert_eq!(color & 0xFFFF, 0, "the texel has no green or blue");
    }

    #[test]
    fn transparent_pixels_leave_the_surface_alone() {
        let canvas = Canvas::default();
        let vertices = [
            vertex(0.0, 0.0, 0x0000_0000),
            vertex(8.0, 0.0, 0x0000_0000),
            vertex(0.0, 8.0, 0x0000_0000),
        ];
        let written = fill_triangle(&canvas, &target(), vertices, [None; 4], over_blend(), None);

        assert_eq!(written, 0, "nothing is drawn through zero alpha");
        assert!(canvas.0.lock().expect("lock").is_empty());
    }

    #[test]
    fn half_alpha_mixes_with_the_destination() {
        let canvas = Canvas::default();
        // Paint the surface white, then draw half-transparent black over it.
        canvas.write_pixel(0x1000, 0xFFFF_FFFF);
        let vertices = [
            vertex(0.0, 0.0, 0x8000_0000),
            vertex(4.0, 0.0, 0x8000_0000),
            vertex(0.0, 4.0, 0x8000_0000),
        ];
        fill_triangle(&canvas, &target(), vertices, [None; 4], over_blend(), None);

        let pixels = canvas.0.lock().expect("lock");
        let (_, color) = *pixels
            .iter()
            .rev()
            .find(|(address, _)| *address == 0x1000)
            .expect("the painted pixel was drawn over");
        let red = (color >> 16) & 0xFF;
        assert!((0x78..=0x88).contains(&red), "about half of white remains: {red:#x}");
    }

    #[test]
    fn a_fragment_behind_the_surface_is_rejected() {
        let canvas = Canvas::default();
        let target = RenderTarget {
            base: 0x1000,
            pitch: 40,
            clip_x: 0,
            clip_y: 0,
            width: 10,
            height: 10,
            zeta: Some(0x8000),
            zeta_pitch: 40,
        };
        // The surface already holds a nearer depth than the fragment's.
        canvas.write_pixel(0x8000, 100 << 8);
        let mut vertices = [
            vertex(0.0, 0.0, 0xFFFFFFFF),
            vertex(4.0, 0.0, 0xFFFFFFFF),
            vertex(0.0, 4.0, 0xFFFFFFFF),
        ];
        for vertex in &mut vertices {
            vertex.z = 500.0;
        }
        let state = PipelineState {
            depth: DepthState { test: true, write: true, function: 3 },
            ..PipelineState::default()
        };
        fill_triangle(&canvas, &target, vertices, [None; 4], state, None);

        let pixels = canvas.0.lock().expect("lock");
        assert!(
            !pixels.iter().skip(1).any(|(address, _)| *address == 0x1000),
            "the first pixel is behind what is already there"
        );
    }

    #[test]
    fn a_nearer_fragment_draws_and_updates_the_depth() {
        let canvas = Canvas::default();
        let target = RenderTarget {
            base: 0x1000,
            pitch: 40,
            clip_x: 0,
            clip_y: 0,
            width: 10,
            height: 10,
            zeta: Some(0x8000),
            zeta_pitch: 40,
        };
        canvas.write_pixel(0x8000, 500 << 8);
        let mut vertices = [
            vertex(0.0, 0.0, 0xFFFFFFFF),
            vertex(4.0, 0.0, 0xFFFFFFFF),
            vertex(0.0, 4.0, 0xFFFFFFFF),
        ];
        for vertex in &mut vertices {
            vertex.z = 100.0;
        }
        let state = PipelineState {
            depth: DepthState { test: true, write: true, function: 3 },
            ..PipelineState::default()
        };
        let written = fill_triangle(&canvas, &target, vertices, [None; 4], state, None);

        assert!(written > 0, "a nearer fragment draws");
        let pixels = canvas.0.lock().expect("lock");
        let depth_write = pixels.iter().rev().find(|(address, _)| *address == 0x8000);
        assert_eq!(depth_write.expect("the depth surface was updated").1 >> 8, 100);
    }

    #[test]
    fn texture_coordinates_interpolate_in_the_triangle_plane() {
        let canvas = Canvas::default();
        // A triangle whose far vertex has a tenth of the near one's
        // reciprocal w: halfway across in screen space is nearer the near
        // vertex in texture space.
        let mut vertices = [
            vertex(0.0, 0.0, 0xFFFFFFFF),
            vertex(9.0, 0.0, 0xFFFFFFFF),
            vertex(0.0, 9.0, 0xFFFFFFFF),
        ];
        vertices[1].texcoords[0][0] = 1.0;
        vertices[1].inverse_w = 0.1;
        fill_triangle(
            &canvas,
            &target(),
            vertices,
            [Some(&Checker), None, None, None],
            PipelineState::default(),
            None,
        );

        let pixels = canvas.0.lock().expect("lock");
        // With a perspective divide the midpoint still samples the first
        // texel; interpolating in screen space would have reached the
        // second by now.
        let midpoint = pixels
            .iter()
            .find(|(address, _)| *address == 0x1000 + 4 * 4)
            .expect("the midpoint was drawn");
        assert_eq!(midpoint.1, 0xFFFF_0000, "still the near vertex's texel");
    }

    #[test]
    fn an_additive_blend_adds_light_instead_of_replacing_it() {
        let canvas = Canvas::default();
        // A grey surface with a grey pass added over it: the result is
        // brighter than either, which an over-blend could never produce.
        canvas.write_pixel(0x1000, 0xFF40_4040);
        let state = PipelineState {
            blend: BlendState { enabled: true, source: BLEND_ONE, destination: BLEND_ONE },
            ..PipelineState::default()
        };
        let vertices = [
            vertex(0.0, 0.0, 0x0040_4040),
            vertex(4.0, 0.0, 0x0040_4040),
            vertex(0.0, 4.0, 0x0040_4040),
        ];
        fill_triangle(&canvas, &target(), vertices, [None; 4], state, None);

        let pixels = canvas.0.lock().expect("lock");
        let (_, color) = *pixels
            .iter()
            .rev()
            .find(|(address, _)| *address == 0x1000)
            .expect("the pixel was drawn");
        assert_eq!((color >> 16) & 0xFF, 0x80, "two greys add to a brighter one");
    }

    #[test]
    fn a_transparent_additive_pass_still_adds() {
        let canvas = Canvas::default();
        canvas.write_pixel(0x1000, 0xFF00_0000);
        let state = PipelineState {
            blend: BlendState { enabled: true, source: BLEND_ONE, destination: BLEND_ONE },
            ..PipelineState::default()
        };
        // Zero alpha, but additive: the colour still reaches the surface.
        let vertices = [
            vertex(0.0, 0.0, 0x0020_2020),
            vertex(4.0, 0.0, 0x0020_2020),
            vertex(0.0, 4.0, 0x0020_2020),
        ];
        let written = fill_triangle(&canvas, &target(), vertices, [None; 4], state, None);

        assert!(written > 0, "an additive pass is not skipped for its alpha");
    }

    #[test]
    fn culling_discards_one_winding_and_keeps_the_other() {
        let canvas = Canvas::default();
        let state = PipelineState {
            cull: CullState { enabled: true, face: 0x0405, front_face: 0x0900 },
            ..PipelineState::default()
        };
        let clockwise = [
            vertex(1.0, 1.0, 0xFFFFFFFF),
            vertex(8.0, 1.0, 0xFFFFFFFF),
            vertex(1.0, 8.0, 0xFFFFFFFF),
        ];
        let counter = [clockwise[0], clockwise[2], clockwise[1]];
        let first = fill_triangle(&canvas, &target(), clockwise, [None; 4], state, None);
        let second = fill_triangle(&canvas, &target(), counter, [None; 4], state, None);

        assert!(first > 0 || second > 0, "one winding survives");
        assert_eq!(first.min(second), 0, "and the other is discarded");
    }

    #[test]
    fn the_alpha_test_discards_what_fails_it() {
        let canvas = Canvas::default();
        // Greater than 0x80: a fragment at 0x40 fails, one at 0xFF passes.
        let state = PipelineState {
            alpha: AlphaTest { enabled: true, function: 4, reference: 0x80 },
            ..PipelineState::default()
        };
        let below = [
            vertex(0.0, 0.0, 0x4000_0000),
            vertex(4.0, 0.0, 0x4000_0000),
            vertex(0.0, 4.0, 0x4000_0000),
        ];
        let above = [
            vertex(0.0, 0.0, 0xFFFF_FFFF),
            vertex(4.0, 0.0, 0xFFFF_FFFF),
            vertex(0.0, 4.0, 0xFFFF_FFFF),
        ];
        assert_eq!(fill_triangle(&canvas, &target(), below, [None; 4], state, None), 0);
        assert!(fill_triangle(&canvas, &target(), above, [None; 4], state, None) > 0);
    }

    #[test]
    fn a_clip_offset_moves_the_drawable_area() {
        let canvas = Canvas::default();
        let target = RenderTarget {
            base: 0x1000,
            pitch: 40,
            clip_x: 5,
            clip_y: 5,
            width: 5,
            height: 5,
            zeta: None,
            zeta_pitch: 0,
        };
        let written = fill_triangle(
            &canvas,
            &target,
            [
                vertex(0.0, 0.0, 0xFFFFFFFF),
                vertex(10.0, 0.0, 0xFFFFFFFF),
                vertex(0.0, 10.0, 0xFFFFFFFF),
            ],
            [None; 4],
            PipelineState::default(),
            None,
        );
        // Only the part of the triangle inside the clip is drawn, and the
        // clip's own corner is outside the triangle's half-space.
        assert!(written < 25, "the clip bounds the fill: {written}");
        let pixels = canvas.0.lock().expect("lock");
        assert!(pixels.iter().all(|(address, _)| *address >= 0x1000 + 5 * 40 + 5 * 4));
    }
}
