//! A decoded-instruction cache for the interpreter.
//!
//! Decoding costs more than executing for most instructions: every step
//! fetched up to fifteen bytes through the checked-memory path and ran the
//! full decoder over them. Guest code barely changes, so the decode is
//! cached per address and validated the way the code cache validates
//! translations (ADR 0005): by the physical page's generation, which every
//! guest write bumps. Self-modifying code therefore redecodes exactly when
//! it must, via one atomic load per cached page instead of a fresh decode.
//!
//! The cache is direct-mapped: a collision simply redecodes, and the
//! replacement policy being trivial keeps the hit path to an index, a tag
//! compare, and the generation checks.

use iced_x86::Instruction;

use exbawks_memory::{GuestMemory, PageTable};
use exbawks_types::{GUEST_PAGE_SIZE, GuestPage};

/// One cached decode: the address it belongs to, the pages its bytes came
/// from with their generations at decode time, and the instruction.
#[derive(Clone)]
struct CacheEntry {
    /// The instruction's address, or `u32::MAX` for an empty slot (no
    /// instruction can be fetched from the last byte of the address space).
    eip: u32,
    /// The physical pages the instruction bytes occupied (an instruction
    /// can straddle two), and each page's generation when it was decoded.
    pages: [(GuestPage, u16); 2],
    /// How many of `pages` are meaningful.
    page_count: u8,
    /// The decoded instruction.
    instruction: Instruction,
}

impl Default for CacheEntry {
    fn default() -> Self {
        Self {
            eip: u32::MAX,
            pages: [(GuestPage(0), 0); 2],
            page_count: 0,
            instruction: Instruction::default(),
        }
    }
}

/// A direct-mapped cache of decoded instructions.
pub struct InstructionCache {
    entries: Vec<CacheEntry>,
}

/// The cache's size in entries; a power of two, so the index is a mask.
const CACHE_ENTRIES: usize = 1 << 16;

impl Default for InstructionCache {
    fn default() -> Self {
        Self::new()
    }
}

impl InstructionCache {
    /// Creates an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self { entries: vec![CacheEntry::default(); CACHE_ENTRIES] }
    }

    fn slot(eip: u32) -> usize {
        (eip as usize) & (CACHE_ENTRIES - 1)
    }

    /// The cached decode for `eip`, when its pages are unchanged.
    ///
    /// Two things must both hold: the virtual pages still map the same
    /// physical pages they mapped at decode time (a remap silently points
    /// the address at different bytes), and those pages' generations are
    /// unchanged (a write through any alias bumps them, ADR 0005).
    pub fn get(&self, eip: u32, table: &PageTable) -> Option<Instruction> {
        let entry = &self.entries[Self::slot(eip)];
        if entry.eip != eip {
            return None;
        }
        for (index, (page, generation)) in
            entry.pages[..entry.page_count as usize].iter().enumerate()
        {
            let virtual_page = GuestPage(eip / GUEST_PAGE_SIZE + index as u32);
            if table.get(virtual_page).physical_page() != *page {
                return None;
            }
            if table.physical_generation(*page) != Some(*generation) {
                return None;
            }
        }
        Some(entry.instruction)
    }

    /// Records a decode, capturing the pages it depends on.
    pub fn put(&mut self, eip: u32, instruction: Instruction, memory: &dyn GuestMemory) {
        let table = memory.page_table();
        let mut pages = [(GuestPage(0), 0_u16); 2];
        let mut page_count = 0_u8;
        let first = eip / GUEST_PAGE_SIZE;
        let last = eip.wrapping_add(instruction.len() as u32).wrapping_sub(1) / GUEST_PAGE_SIZE;
        for virtual_page in first..=last.max(first) {
            let descriptor = table.get(GuestPage(virtual_page));
            // Only plain RAM carries a generation; an MMIO descriptor's
            // page field is a device id, and validating against the
            // generation of whatever page shares that number would let a
            // stale decode survive. Anything else is simply not cached.
            if descriptor.kind() != exbawks_memory::PageKind::Ram {
                return;
            }
            let physical = descriptor.physical_page();
            let Some(generation) = table.physical_generation(physical) else {
                return;
            };
            pages[page_count as usize] = (physical, generation);
            page_count += 1;
            if page_count == 2 {
                break;
            }
        }
        self.entries[Self::slot(eip)] = CacheEntry { eip, pages, page_count, instruction };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_cache_misses() {
        let cache = InstructionCache::new();
        let table = PageTable::new();
        assert!(cache.get(0x1_0000, &table).is_none());
    }
}
