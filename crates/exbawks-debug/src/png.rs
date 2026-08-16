//! A minimal PNG encoder for frame captures.
//!
//! Screenshots are a testing artifact, not a delivery format: what matters
//! is that a captured frame lands in a file any viewer opens and hashes
//! identically for the same pixels. That needs no compression, so this
//! writes deflate *stored* blocks — a few dozen lines with no dependency,
//! deterministic byte-for-byte, and readable by every PNG decoder.

/// The PNG signature bytes.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// The largest payload one deflate stored block carries.
const STORED_BLOCK_MAX: usize = 0xFFFF;

/// Encodes 8-bit RGBA pixels as a PNG image.
///
/// `pixels` is row-major, four bytes per pixel, `width * height * 4` bytes
/// long; a shorter slice reads as transparent black beyond its end so a
/// truncated capture still produces an image.
#[must_use]
pub fn encode_rgba(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    for row in 0..height as usize {
        // Filter type 0 (none) prefixes every scanline.
        raw.push(0);
        let start = row * width as usize * 4;
        let end = start + width as usize * 4;
        let available = pixels.get(start..end.min(pixels.len())).unwrap_or(&[]);
        raw.extend_from_slice(available);
        raw.resize(raw.len() + (end - start - available.len()), 0);
    }

    let mut png = Vec::from(SIGNATURE);
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace.
    write_chunk(&mut png, b"IHDR", &header);
    write_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    write_chunk(&mut png, b"IEND", &[]);
    png
}

/// Wraps data in a zlib stream of uncompressed deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    // Deflate compression method, 32 KiB window, no preset dictionary; the
    // header's two bytes must be a multiple of 31.
    let mut out = vec![0x78, 0x01];
    let mut offset = 0;
    loop {
        let chunk = &data[offset..(offset + STORED_BLOCK_MAX).min(data.len())];
        offset += chunk.len();
        let final_block = u8::from(offset >= data.len());
        out.push(final_block);
        let length = chunk.len() as u16;
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&(!length).to_le_bytes());
        out.extend_from_slice(chunk);
        if final_block == 1 {
            break;
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Appends one length-tagged, CRC-checked PNG chunk.
fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = crc32(kind);
    crc = crc32_continue(crc, data);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// The zlib Adler-32 checksum.
fn adler32(data: &[u8]) -> u32 {
    let (mut low, mut high) = (1_u32, 0_u32);
    for byte in data {
        low = (low + u32::from(*byte)) % 65521;
        high = (high + low) % 65521;
    }
    (high << 16) | low
}

/// The PNG CRC-32 of one buffer.
fn crc32(data: &[u8]) -> u32 {
    crc32_continue(0, data)
}

/// Continues a finished CRC-32 over more bytes.
///
/// Values are stored finalized (inverted), so a chained call re-inverts the
/// running remainder first: `crc32_continue(crc32(a), b) == crc32(a ++ b)`.
fn crc32_continue(previous: u32, data: &[u8]) -> u32 {
    let mut crc = !previous;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_encoded_image_has_the_signature_and_chunks() {
        let pixels = vec![0xFF; 4 * 4 * 4];
        let png = encode_rgba(4, 4, &pixels);
        assert_eq!(&png[..8], &SIGNATURE, "the signature leads the file");
        assert!(png.windows(4).any(|window| window == b"IHDR"));
        assert!(png.windows(4).any(|window| window == b"IDAT"));
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND", "IEND closes it");
    }

    #[test]
    fn the_header_carries_the_dimensions() {
        let png = encode_rgba(640, 480, &[]);
        // IHDR data begins after the signature, length, and chunk type.
        let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let height = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        assert_eq!((width, height), (640, 480));
    }

    #[test]
    fn the_same_pixels_encode_identically() {
        let pixels: Vec<u8> = (0..64 * 4).map(|index| (index % 251) as u8).collect();
        assert_eq!(encode_rgba(8, 8, &pixels), encode_rgba(8, 8, &pixels), "goldens are stable");
    }

    #[test]
    fn a_large_image_spans_several_stored_blocks() {
        // Each row is 4 KiB + 1; 64 rows exceed one 64 KiB stored block.
        let pixels = vec![0x20; 1024 * 64 * 4];
        let png = encode_rgba(1024, 64, &pixels);
        assert!(png.len() > STORED_BLOCK_MAX, "the payload needed more than one block");
    }

    #[test]
    fn a_short_pixel_buffer_pads_with_zeroes() {
        let png = encode_rgba(4, 4, &[0xFF; 8]);
        assert!(!png.is_empty(), "a truncated capture still encodes");
    }

    #[test]
    fn the_checksums_match_known_values() {
        // The canonical Adler-32 and CRC-32 of "123456789".
        assert_eq!(adler32(b"123456789"), 0x091E_01DE);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        // Chaining a finished value continues the same remainder.
        assert_eq!(crc32_continue(crc32(b"12345"), b"6789"), crc32(b"123456789"));
    }
}
