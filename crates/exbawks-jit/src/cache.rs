use std::collections::HashMap;
use std::sync::Arc;

use exbawks_memory::PageTable;
use exbawks_types::{BackendKind, GuestPage, GuestVa};
use parking_lot::RwLock;

use crate::CompiledBlock;

/// A lookup key for one translated guest block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockKey {
    /// The first guest instruction address.
    pub guest_start: GuestVa,
    /// The address-space mapping epoch.
    pub address_space_epoch: u64,
    /// The selected codegen backend.
    pub backend: BackendKind,
}

/// One physical code-page generation captured during compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalPageDependency {
    /// The guest physical page.
    pub page: GuestPage,
    /// The captured generation value.
    pub generation: u16,
}

/// One code-cache entry and its physical-page dependencies.
#[derive(Debug, Clone)]
pub struct CachedBlock {
    /// The translated block.
    pub block: Arc<CompiledBlock>,
    /// The captured physical-page generations.
    pub dependencies: Vec<PhysicalPageDependency>,
}

impl CachedBlock {
    /// Returns true when every dependency still matches the page table.
    #[must_use]
    pub fn generations_match(&self, table: &PageTable) -> bool {
        self.dependencies.iter().all(|dependency| {
            table
                .physical_generation(dependency.page)
                .is_some_and(|generation| generation == dependency.generation)
        })
    }
}

/// A synchronized translated block cache.
#[derive(Debug, Default)]
pub struct CodeCache {
    entries: RwLock<HashMap<BlockKey, CachedBlock>>,
}

impl CodeCache {
    /// Inserts or replaces one translated block.
    pub fn insert(
        &self,
        key: BlockKey,
        block: Arc<CompiledBlock>,
        dependencies: Vec<PhysicalPageDependency>,
    ) {
        self.entries.write().insert(key, CachedBlock { block, dependencies });
    }

    /// Returns a valid cached block.
    #[must_use]
    pub fn get(&self, key: BlockKey, table: &PageTable) -> Option<Arc<CompiledBlock>> {
        let entry = self.entries.read().get(&key).cloned()?;
        entry.generations_match(table).then_some(entry.block)
    }

    /// Removes every entry that depends on one physical page.
    pub fn invalidate_physical_page(&self, page: GuestPage) -> usize {
        let mut entries = self.entries.write();
        let before = entries.len();
        entries.retain(|_, value| {
            !value.dependencies.iter().any(|dependency| dependency.page == page)
        });
        before - entries.len()
    }

    /// Returns the current entry count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Returns true when the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Removes all entries.
    pub fn clear(&self) {
        self.entries.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::BasicBlockDecoder;
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, MemoryPermissions};

    use crate::{CodegenBackend, DirectRewriteBackend};

    use super::*;

    #[test]
    fn cache_invalidates_by_physical_page() {
        let table = PageTable::new();
        let range = GuestRange::page_aligned(GuestVa(0x1000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        table
            .map_ram(range, GuestPage(3), MemoryPermissions::READ | MemoryPermissions::EXECUTE)
            .expect("mapping succeeds");

        let decoded =
            BasicBlockDecoder::default().decode(GuestVa(0x1000), &[0xC3]).expect("block decodes");
        let block =
            Arc::new(DirectRewriteBackend::default().compile(&decoded).expect("plan succeeds"));
        let cache = CodeCache::default();
        let key = BlockKey {
            guest_start: GuestVa(0x1000),
            address_space_epoch: 0,
            backend: BackendKind::DirectRewrite,
        };
        cache.insert(
            key,
            block,
            vec![PhysicalPageDependency { page: GuestPage(3), generation: 0 }],
        );

        assert!(cache.get(key, &table).is_some());
        assert_eq!(table.bump_physical_generation(GuestPage(3)), Some(1));
        assert!(cache.get(key, &table).is_none());
        assert_eq!(cache.invalidate_physical_page(GuestPage(3)), 1);
        assert!(cache.is_empty());
    }
}
