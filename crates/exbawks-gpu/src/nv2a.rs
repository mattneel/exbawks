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

/// The engine's per-channel decode state.
#[derive(Debug, Default)]
struct ChannelState {
    /// The object handle bound to each of the eight subchannels.
    subchannel_handles: [u32; 8],
    /// The semaphore context-DMA object, once bound.
    semaphore: Option<DmaObject>,
    /// The current semaphore byte offset.
    semaphore_offset: u32,
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

/// Per-submission dword budget: a runaway or circular pushbuffer must not
/// hang the emulator.
const MAX_DWORDS_PER_SUBMIT: u32 = 4 * 1024 * 1024;

/// The `SET_OBJECT` method (binds a handle to a subchannel).
const METHOD_SET_OBJECT: u16 = 0x0000;
/// Kelvin `SET_CONTEXT_DMA_SEMAPHORE` (provisional numbering; verified
/// against the live retail stream, which names its methods by use).
const METHOD_SET_CONTEXT_DMA_SEMAPHORE: u16 = 0x01A4;
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
