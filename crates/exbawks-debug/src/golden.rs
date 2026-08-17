//! Frame digests for golden testing.
//!
//! A captured frame is only useful as a regression test if the same guest
//! run produces the same bytes and a change is visible as a difference.
//! Titles cannot be committed, so a golden is a digest recorded alongside
//! the private image it came from: this hashes a frame to a short, stable
//! string, and reports how two frames differ when they should not.

/// The offset basis of the 64-bit FNV-1a hash.
const FNV_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
/// Its prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;

/// Digests a frame's pixels, dimensions included.
///
/// The dimensions take part so a frame cannot match a differently shaped
/// one whose bytes happen to agree.
#[must_use]
pub fn frame_digest(width: u32, height: u32, pixels: &[u8]) -> String {
    let mut hash = FNV_OFFSET;
    let mut absorb = |byte: u8| {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    };
    for byte in width.to_le_bytes() {
        absorb(byte);
    }
    for byte in height.to_le_bytes() {
        absorb(byte);
    }
    for byte in pixels {
        absorb(*byte);
    }
    format!("{hash:016x}")
}

/// How two frames of the same shape differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDifference {
    /// Pixels whose bytes differ.
    pub differing_pixels: u64,
    /// The largest difference in any one channel.
    pub largest_channel_difference: u8,
}

/// Compares two frames pixel by pixel.
///
/// Returns `None` when the frames are different lengths, which is a
/// difference no per-pixel report describes.
#[must_use]
pub fn compare_frames(left: &[u8], right: &[u8]) -> Option<FrameDifference> {
    if left.len() != right.len() {
        return None;
    }
    let mut differing = 0;
    let mut largest = 0;
    for (a, b) in left.chunks_exact(4).zip(right.chunks_exact(4)) {
        if a == b {
            continue;
        }
        differing += 1;
        for (left_channel, right_channel) in a.iter().zip(b) {
            largest = largest.max(left_channel.abs_diff(*right_channel));
        }
    }
    Some(FrameDifference { differing_pixels: differing, largest_channel_difference: largest })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_frame_digests_the_same_way() {
        let pixels: Vec<u8> = (0..64_u32).map(|value| value as u8).collect();
        assert_eq!(frame_digest(4, 4, &pixels), frame_digest(4, 4, &pixels));
    }

    #[test]
    fn one_changed_byte_changes_the_digest() {
        let pixels: Vec<u8> = vec![7; 64];
        let mut changed = pixels.clone();
        changed[33] = 8;
        assert_ne!(frame_digest(4, 4, &pixels), frame_digest(4, 4, &changed));
    }

    #[test]
    fn the_shape_takes_part_in_the_digest() {
        let pixels: Vec<u8> = vec![3; 64];
        assert_ne!(frame_digest(4, 4, &pixels), frame_digest(2, 8, &pixels));
    }

    #[test]
    fn a_digest_is_sixteen_hexadecimal_characters() {
        let digest = frame_digest(1, 1, &[0, 0, 0, 0]);
        assert_eq!(digest.len(), 16);
        assert!(digest.chars().all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn identical_frames_report_no_difference() {
        let pixels = vec![0x40; 32];
        let difference = compare_frames(&pixels, &pixels).expect("same length");
        assert_eq!(difference.differing_pixels, 0);
        assert_eq!(difference.largest_channel_difference, 0);
    }

    #[test]
    fn a_difference_reports_its_extent() {
        let left = vec![0x40; 32];
        let mut right = left.clone();
        right[4] = 0x50;
        right[9] = 0x30;
        let difference = compare_frames(&left, &right).expect("same length");
        assert_eq!(difference.differing_pixels, 2, "two pixels carry the changed bytes");
        assert_eq!(difference.largest_channel_difference, 0x10);
    }

    #[test]
    fn frames_of_different_lengths_do_not_compare() {
        assert!(compare_frames(&[0; 8], &[0; 12]).is_none());
    }
}
