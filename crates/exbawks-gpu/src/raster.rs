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
    /// The horizontal texture coordinate, in units of the texture width.
    pub u: f32,
    /// The vertical texture coordinate, in units of the texture height.
    pub v: f32,
    /// The depth, in the units the depth surface stores.
    pub z: f32,
    /// The reciprocal of the clip-space `w`, for perspective-correct
    /// interpolation. One for geometry that arrives already projected.
    pub inverse_w: f32,
}

/// A bound texture the rasterizer can sample.
pub trait TextureSource {
    /// The texel at integer coordinates, as 8-bit ARGB.
    ///
    /// Coordinates are already clamped to the texture's extent.
    fn texel(&self, x: u32, y: u32) -> u32;

    /// The texture's width in texels.
    fn width(&self) -> u32;

    /// The texture's height in texels.
    fn height(&self) -> u32;
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
pub trait PixelSink {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    /// The drawn pixel replaces the destination.
    #[default]
    Replace,
    /// The drawn pixel is weighted by its own alpha and the destination by
    /// the remainder — the combination a title's transparent art needs, and
    /// what its interface is authored against.
    SourceAlpha,
}

/// Combines a source and destination color by a blend mode.
fn combine(mode: BlendMode, source: u32, destination: u32) -> u32 {
    match mode {
        BlendMode::Replace => source,
        BlendMode::SourceAlpha => {
            let alpha = (source >> 24) & 0xFF;
            let mut out = 0xFF00_0000;
            for shift in [0, 8, 16] {
                let from = (source >> shift) & 0xFF;
                let onto = (destination >> shift) & 0xFF;
                let value = (from * alpha + onto * (255 - alpha) + 127) / 255;
                out |= (value & 0xFF) << shift;
            }
            out
        }
    }
}

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
    texture: Option<&dyn TextureSource>,
    blend: BlendMode,
    depth: DepthState,
) -> u64 {
    let [a, b, c] = vertices;
    let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
    if area == 0.0 || !area.is_finite() {
        return 0;
    }
    // A back-facing triangle is drawn the same way; culling is the
    // pipeline's decision, not the rasterizer's.
    let (a, b, c) = if area < 0.0 { (a, c, b) } else { (a, b, c) };
    let area = area.abs();

    let left = a.x.min(b.x).min(c.x).floor().max(target.clip_x as f32);
    let right = a.x.max(b.x).max(c.x).ceil().min((target.clip_x + target.width) as f32);
    let top = a.y.min(b.y).min(c.y).floor().max(target.clip_y as f32);
    let bottom = a.y.max(b.y).max(c.y).ceil().min((target.clip_y + target.height) as f32);
    if !(left < right && top < bottom) {
        return 0;
    }

    let mut written = 0;
    let mut y = top as u32;
    let last_y = bottom as u32;
    let first_x = left as u32;
    let last_x = right as u32;
    while y < last_y {
        let center_y = y as f32 + 0.5;
        for x in first_x..last_x {
            let center_x = x as f32 + 0.5;
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

            let mut color = interpolate_color([a.color, b.color, c.color], weights);
            if let Some(texture) = texture {
                // Texture coordinates interpolate in the plane of the
                // triangle, not the screen: weight each by the vertex's
                // reciprocal `w` and divide by the interpolated reciprocal,
                // or a textured surface seen at an angle skews.
                let inverse_w =
                    weights[0] * a.inverse_w + weights[1] * b.inverse_w + weights[2] * c.inverse_w;
                let scale = if inverse_w == 0.0 { 1.0 } else { 1.0 / inverse_w };
                let u = (weights[0] * a.u * a.inverse_w
                    + weights[1] * b.u * b.inverse_w
                    + weights[2] * c.u * c.inverse_w)
                    * scale;
                let v = (weights[0] * a.v * a.inverse_w
                    + weights[1] * b.v * b.inverse_w
                    + weights[2] * c.v * c.inverse_w)
                    * scale;
                let texel_x = (u * texture.width() as f32) as i64;
                let texel_y = (v * texture.height() as f32) as i64;
                let texel = texture.texel(
                    texel_x.clamp(0, i64::from(texture.width().saturating_sub(1))) as u32,
                    texel_y.clamp(0, i64::from(texture.height().saturating_sub(1))) as u32,
                );
                color = modulate(texel, color);
            }
            if blend == BlendMode::SourceAlpha {
                let alpha = (color >> 24) & 0xFF;
                if alpha == 0 {
                    // Fully transparent art must not touch the surface: its
                    // color channels are black, and writing them would
                    // erase whatever the title drew underneath.
                    continue;
                }
                if alpha != 0xFF {
                    color = combine(blend, color, sink.read_pixel(address));
                }
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

    fn vertex(x: f32, y: f32, color: u32) -> ScreenVertex {
        ScreenVertex { x, y, color, u: 0.0, v: 0.0, z: 0.0, inverse_w: 1.0 }
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
            None,
            BlendMode::Replace,
            DepthState::default(),
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
            None,
            BlendMode::Replace,
            DepthState::default(),
        );
        let counter = fill_triangle(
            &canvas,
            &target(),
            [
                vertex(1.0, 1.0, 0xFF00FF00),
                vertex(1.0, 8.0, 0xFF00FF00),
                vertex(8.0, 1.0, 0xFF00FF00),
            ],
            None,
            BlendMode::Replace,
            DepthState::default(),
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
            None,
            BlendMode::Replace,
            DepthState::default(),
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
            None,
            BlendMode::Replace,
            DepthState::default(),
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
            None,
            BlendMode::Replace,
            DepthState::default(),
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
            None,
            BlendMode::Replace,
            DepthState::default(),
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
        vertices[1].u = 1.0;
        vertices[2].v = 1.0;
        let written = fill_triangle(
            &canvas,
            &target(),
            vertices,
            Some(&Checker),
            BlendMode::Replace,
            DepthState::default(),
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
            Some(&Checker),
            BlendMode::Replace,
            DepthState::default(),
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
        let written = fill_triangle(
            &canvas,
            &target(),
            vertices,
            None,
            BlendMode::SourceAlpha,
            DepthState::default(),
        );

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
        fill_triangle(
            &canvas,
            &target(),
            vertices,
            None,
            BlendMode::SourceAlpha,
            DepthState::default(),
        );

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
        let depth = DepthState { test: true, write: true, function: 3 };
        fill_triangle(&canvas, &target, vertices, None, BlendMode::Replace, depth);

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
        let depth = DepthState { test: true, write: true, function: 3 };
        let written = fill_triangle(&canvas, &target, vertices, None, BlendMode::Replace, depth);

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
        vertices[1].u = 1.0;
        vertices[1].inverse_w = 0.1;
        fill_triangle(
            &canvas,
            &target(),
            vertices,
            Some(&Checker),
            BlendMode::Replace,
            DepthState::default(),
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
            None,
            BlendMode::Replace,
            DepthState::default(),
        );
        // Only the part of the triangle inside the clip is drawn, and the
        // clip's own corner is outside the triangle's half-space.
        assert!(written < 25, "the clip bounds the fill: {written}");
        let pixels = canvas.0.lock().expect("lock");
        assert!(pixels.iter().all(|(address, _)| *address >= 0x1000 + 5 * 40 + 5 * 4));
    }
}
