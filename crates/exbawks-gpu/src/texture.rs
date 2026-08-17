//! Block-compressed texture decoding.
//!
//! A title's art arrives compressed: `DXT1` for opaque or one-bit-alpha
//! images, `DXT3` and `DXT5` where alpha varies. All three store 4x4 texel
//! blocks — a color block of two `RGB565` endpoints plus two-bit indices,
//! preceded in the larger formats by eight bytes of alpha — so decoding one
//! texel means finding its block and interpolating within it.

/// The bytes one `DXT1` block occupies.
pub const DXT1_BLOCK_BYTES: u32 = 8;
/// The bytes one `DXT3` or `DXT5` block occupies.
pub const DXT_ALPHA_BLOCK_BYTES: u32 = 16;

/// Expands a `RGB565` value to 8-bit ARGB with full alpha.
fn expand_565(value: u16) -> u32 {
    let red = u32::from((value >> 11) & 0x1F);
    let green = u32::from((value >> 5) & 0x3F);
    let blue = u32::from(value & 0x1F);
    // Replicate the high bits into the low ones so white stays white.
    let red = (red << 3) | (red >> 2);
    let green = (green << 2) | (green >> 4);
    let blue = (blue << 3) | (blue >> 2);
    0xFF00_0000 | (red << 16) | (green << 8) | blue
}

/// Mixes two colors by `numerator/denominator`, channel by channel.
fn mix(first: u32, second: u32, numerator: u32, denominator: u32) -> u32 {
    let mut out = 0xFF00_0000;
    for shift in [0, 8, 16] {
        let a = (first >> shift) & 0xFF;
        let b = (second >> shift) & 0xFF;
        let value = (a * (denominator - numerator) + b * numerator) / denominator;
        out |= (value & 0xFF) << shift;
    }
    out
}

/// Decodes one texel of a `DXT1` color block.
///
/// `block` is the eight bytes as two little-endian dwords: the endpoint
/// pair and the index bits. When the first endpoint does not exceed the
/// second, the block carries three colors and a transparent slot.
#[must_use]
pub fn dxt1_texel(block: [u32; 2], x: u32, y: u32) -> u32 {
    let first = (block[0] & 0xFFFF) as u16;
    let second = ((block[0] >> 16) & 0xFFFF) as u16;
    color_texel(block, x, y, first > second)
}

/// Decodes one texel of a color block that always carries four colors.
///
/// `DXT3` and `DXT5` keep alpha in their own half of the block, so their
/// color halves never spend a slot on transparency the way a `DXT1` block
/// can — reading them by `DXT1`'s rule turns a quarter of the texels of any
/// block whose endpoints happen to ascend into transparent black.
#[must_use]
pub fn dxt_opaque_texel(block: [u32; 2], x: u32, y: u32) -> u32 {
    color_texel(block, x, y, true)
}

/// Decodes one texel of a color block under the chosen interpretation.
fn color_texel(block: [u32; 2], x: u32, y: u32, four_colors: bool) -> u32 {
    let color0 = expand_565((block[0] & 0xFFFF) as u16);
    let color1 = expand_565(((block[0] >> 16) & 0xFFFF) as u16);
    let index = (block[1] >> ((y & 3) * 8 + (x & 3) * 2)) & 0x3;
    if four_colors {
        match index {
            0 => color0,
            1 => color1,
            2 => mix(color0, color1, 1, 3),
            _ => mix(color0, color1, 2, 3),
        }
    } else {
        match index {
            0 => color0,
            1 => color1,
            2 => mix(color0, color1, 1, 2),
            // The fourth slot is transparent black in the three-color mode.
            _ => 0,
        }
    }
}

/// Decodes one texel's alpha from a `DXT3` explicit-alpha block.
#[must_use]
pub fn dxt3_alpha(block: [u32; 2], x: u32, y: u32) -> u32 {
    let row = block[usize::from(y & 3 >= 2)];
    let nibble = (row >> (((y & 1) * 4 + (x & 3)) * 4)) & 0xF;
    // Four bits scale to eight by replication.
    (nibble << 4) | nibble
}

/// Decodes one texel's alpha from a `DXT5` interpolated-alpha block.
#[must_use]
pub fn dxt5_alpha(block: [u32; 2], x: u32, y: u32) -> u32 {
    let first = block[0] & 0xFF;
    let second = (block[0] >> 8) & 0xFF;
    // The three-bit indices run across a 48-bit field beginning at byte two.
    let bits = u64::from(block[0] >> 16) | (u64::from(block[1]) << 16);
    let shift = ((y & 3) * 4 + (x & 3)) * 3;
    let index = ((bits >> shift) & 0x7) as u32;
    match (index, first > second) {
        (0, _) => first,
        (1, _) => second,
        (index, true) => (first * (8 - index) + second * (index - 1)) / 7,
        (6, false) => 0,
        (7, false) => 255,
        (index, false) => (first * (6 - index) + second * (index - 1)) / 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block of solid white in the four-color mode.
    const WHITE_BLOCK: [u32; 2] = [0xFFFF_FFFF, 0x0000_0000];

    #[test]
    fn a_solid_block_decodes_to_its_endpoint() {
        for (x, y) in [(0, 0), (3, 3), (1, 2)] {
            assert_eq!(dxt1_texel(WHITE_BLOCK, x, y), 0xFFFF_FFFF);
        }
    }

    #[test]
    fn endpoints_expand_with_replicated_bits() {
        // A pure red first endpoint and pure blue second, with the first
        // two texels selecting one endpoint each.
        let block = [0x001F_F800, 0x0000_0004];
        assert_eq!(dxt1_texel(block, 0, 0), 0xFFFF_0000, "index 0 is the first endpoint");
        assert_eq!(dxt1_texel(block, 1, 0), 0xFF00_00FF, "index 1 is the second");
    }

    #[test]
    fn the_four_color_mode_interpolates_by_thirds() {
        // White then black, descending, so the block carries four colors;
        // every index is two, a third of the way from white toward black.
        let block = [0x0000_FFFF, 0xAAAA_AAAA];
        let texel = dxt1_texel(block, 0, 0);
        let red = (texel >> 16) & 0xFF;
        assert!((0xA6..=0xAE).contains(&red), "two thirds white: {red:#x}");
    }

    #[test]
    fn an_alpha_carrying_block_keeps_four_colors() {
        // Ascending endpoints would select the three-color mode in DXT1;
        // a DXT3 or DXT5 color half must not read the fourth slot as clear.
        let block = [0xFFFF_0000, 0xFFFF_FFFF];
        assert_eq!(dxt1_texel(block, 0, 0), 0, "DXT1 spends the slot on transparency");
        assert_ne!(dxt_opaque_texel(block, 0, 0), 0, "an alpha-carrying block does not");
        assert_eq!(dxt_opaque_texel(block, 0, 0) >> 24, 0xFF, "and stays opaque");
    }

    #[test]
    fn the_three_color_mode_has_a_transparent_slot() {
        // Ascending endpoints select the three-color mode; index 3 is clear.
        let block = [0xFFFF_0000, 0xFFFF_FFFF];
        assert_eq!(dxt1_texel(block, 0, 0), 0, "the fourth slot is transparent");
    }

    #[test]
    fn explicit_alpha_replicates_its_nibble() {
        let block = [0x0000_000F, 0];
        assert_eq!(dxt3_alpha(block, 0, 0), 0xFF, "a full nibble is full alpha");
        assert_eq!(dxt3_alpha(block, 1, 0), 0x00);
    }

    #[test]
    fn interpolated_alpha_reads_its_endpoints() {
        // Endpoints 255 and 0, every index zero: the first endpoint.
        assert_eq!(dxt5_alpha([0x0000_00FF, 0], 0, 0), 255);
        // The same endpoints with every three-bit index set to one.
        let block = [0x9249_00FF, 0x2492_4924];
        assert_eq!(dxt5_alpha(block, 0, 0), 0, "index one selects the second endpoint");
    }

    #[test]
    fn interpolated_alpha_spans_the_endpoints() {
        // Endpoints 255 and 0 with every index two: six sevenths of 255.
        let block = [0x2492_00FF, 0x4924_9249];
        let alpha = dxt5_alpha(block, 0, 0);
        assert!((210..=225).contains(&alpha), "six sevenths of the way: {alpha}");
    }
}
