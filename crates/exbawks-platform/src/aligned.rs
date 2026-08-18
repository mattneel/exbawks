//! Page-aligned host memory for guest RAM.
//!
//! The Windows Hypervisor Platform maps host memory into a partition by
//! virtual address, requiring 4 KiB alignment and an address that stays
//! stable for the mapping's lifetime. A plain `Vec<u8>`/`Box<[u8]>` gives
//! neither guarantee, so guest physical RAM lives in this fixed-size,
//! page-aligned allocation instead.

use core::ptr::NonNull;
use std::alloc::Layout;

/// The required allocation alignment (one host/guest page).
const PAGE_ALIGN: usize = 4096;

/// A fixed-size, zero-initialized, 4 KiB-aligned host allocation.
///
/// The allocation never moves or resizes, so its base address is stable for
/// the buffer's whole lifetime — the property a hypervisor mapping needs.
pub struct AlignedBuffer {
    pointer: NonNull<u8>,
    len: usize,
    layout: Layout,
}

// SAFETY: the buffer exclusively owns its allocation; access control is the
// caller's responsibility exactly as with `Box<[u8]>`.
unsafe impl Send for AlignedBuffer {}
// SAFETY: shared references only permit reads, as with `Box<[u8]>`.
unsafe impl Sync for AlignedBuffer {}

impl AlignedBuffer {
    /// Allocates `len` zeroed bytes at page alignment.
    ///
    /// Returns `None` when `len` is zero, not page-granular, or the
    /// allocation fails.
    #[must_use]
    pub fn new_zeroed(len: usize) -> Option<Self> {
        if len == 0 || !len.is_multiple_of(PAGE_ALIGN) {
            return None;
        }
        let layout = Layout::from_size_align(len, PAGE_ALIGN).ok()?;
        // SAFETY: the layout has nonzero size and a valid power-of-two
        // alignment; the allocation is owned by this struct and freed in
        // `Drop` with the same layout.
        let pointer = unsafe { std::alloc::alloc_zeroed(layout) };
        Some(Self { pointer: NonNull::new(pointer)?, len, layout })
    }

    /// The stable base address of the allocation.
    #[must_use]
    pub fn base_ptr(&self) -> *const u8 {
        self.pointer.as_ptr()
    }

    /// The buffer length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// True when the buffer holds no bytes (never; construction rejects it).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The buffer as dwords that several threads may share.
    ///
    /// Guest RAM is one resource several emulated devices reach at once, and
    /// a rasterizer covering a surface wants to spread its rows across the
    /// host's processors. A plain slice cannot express that: the rows are
    /// disjoint, but they are strided rather than contiguous, so the borrow
    /// checker cannot see it. Viewing RAM as dwords each thread loads and
    /// stores individually says exactly what is true — the memory is shared,
    /// and who writes which part of it is the caller's arrangement.
    ///
    /// This costs nothing on the hosts this targets: a relaxed load or store
    /// of an aligned dword is an ordinary move. It buys the guarantee that
    /// two threads touching neighbouring pixels is defined behaviour rather
    /// than something that merely happens to work.
    ///
    /// The exclusive borrow is what makes the view sound — while it is
    /// alive, no other reference to these bytes can exist.
    pub fn as_atomic_dwords(&mut self) -> &[core::sync::atomic::AtomicU32] {
        // `AtomicU32` has the same size and layout as `u32`, and every bit
        // pattern is a valid one, so the bytes themselves need no
        // conversion; the allocation being page-aligned and page-granular
        // is what makes them aligned and a whole number of dwords.
        //
        // SAFETY: `pointer` addresses `len` live, initialized, owned bytes,
        // aligned for `AtomicU32`; the exclusive borrow of `self` outlives
        // the view, so nothing else references them while it is alive.
        unsafe {
            core::slice::from_raw_parts(
                self.pointer.as_ptr().cast::<core::sync::atomic::AtomicU32>(),
                self.len / 4,
            )
        }
    }
}

impl core::fmt::Debug for AlignedBuffer {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("AlignedBuffer")
            .field("base", &self.pointer)
            .field("len", &self.len)
            .finish()
    }
}

impl core::ops::Deref for AlignedBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: `pointer` addresses `len` live, initialized bytes owned by
        // this struct; the shared borrow of `self` guards aliasing.
        unsafe { core::slice::from_raw_parts(self.pointer.as_ptr(), self.len) }
    }
}

impl core::ops::DerefMut for AlignedBuffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, and the exclusive borrow of `self` guarantees no
        // other Rust reference aliases the bytes.
        unsafe { core::slice::from_raw_parts_mut(self.pointer.as_ptr(), self.len) }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        // SAFETY: `pointer` came from `alloc_zeroed` with exactly `layout`.
        unsafe { std::alloc::dealloc(self.pointer.as_ptr(), self.layout) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_aligned_zeroed_and_stable() {
        let mut buffer = AlignedBuffer::new_zeroed(2 * 4096).expect("allocation succeeds");
        assert_eq!(buffer.len(), 2 * 4096);
        assert_eq!(buffer.base_ptr() as usize % 4096, 0, "page aligned");
        assert!(buffer.iter().all(|byte| *byte == 0), "zero initialized");
        let before = buffer.base_ptr();
        buffer[4096] = 0xAB;
        assert_eq!(buffer.base_ptr(), before, "the address is stable");
        assert_eq!(buffer[4096], 0xAB);
    }

    #[test]
    fn shares_dwords_between_threads() {
        use core::sync::atomic::Ordering;

        let mut buffer = AlignedBuffer::new_zeroed(4096).expect("allocation succeeds");
        let dwords = buffer.as_atomic_dwords();
        assert_eq!(dwords.len(), 1024, "a page holds this many dwords");
        assert_eq!(dwords.as_ptr() as usize % align_of::<u32>(), 0, "aligned");

        // Two threads writing disjoint halves is the arrangement the
        // rasterizer makes of a surface's rows.
        std::thread::scope(|scope| {
            let (low, high) = dwords.split_at(512);
            scope.spawn(|| {
                for (index, slot) in low.iter().enumerate() {
                    slot.store(index as u32, Ordering::Relaxed);
                }
            });
            scope.spawn(|| {
                for (index, slot) in high.iter().enumerate() {
                    slot.store(0xFFFF_0000 | index as u32, Ordering::Relaxed);
                }
            });
        });

        assert_eq!(dwords[0].load(Ordering::Relaxed), 0);
        assert_eq!(dwords[511].load(Ordering::Relaxed), 511);
        assert_eq!(dwords[512].load(Ordering::Relaxed), 0xFFFF_0000);
        // The bytes read back through the ordinary view as little endian.
        assert_eq!(&buffer[2044..2048], &[0xFF, 0x01, 0x00, 0x00]);
    }

    #[test]
    fn rejects_empty_and_unaligned_lengths() {
        assert!(AlignedBuffer::new_zeroed(0).is_none());
        assert!(AlignedBuffer::new_zeroed(4097).is_none());
    }
}
