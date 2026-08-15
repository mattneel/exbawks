use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::{KernelCallContext, KernelError, KernelExport, KernelStatus};

/// A synchronized registry of kernel HLE exports.
#[derive(Default)]
pub struct KernelRegistry {
    exports: RwLock<HashMap<u16, Arc<dyn KernelExport>>>,
}

impl KernelRegistry {
    /// Creates an empty export registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one export.
    pub fn register<E>(&self, export: E) -> Result<(), KernelError>
    where
        E: KernelExport + 'static,
    {
        self.register_shared(Arc::new(export))
    }

    /// Registers one shared export object.
    pub fn register_shared(&self, export: Arc<dyn KernelExport>) -> Result<(), KernelError> {
        let ordinal = export.ordinal();
        let mut exports = self.exports.write();
        match exports.entry(ordinal) {
            Entry::Occupied(_) => Err(KernelError::DuplicateOrdinal { ordinal }),
            Entry::Vacant(entry) => {
                entry.insert(export);
                Ok(())
            }
        }
    }

    /// Calls one registered export.
    pub fn dispatch(
        &self,
        ordinal: u16,
        context: &mut KernelCallContext<'_>,
    ) -> Result<KernelStatus, KernelError> {
        let export = self
            .exports
            .read()
            .get(&ordinal)
            .cloned()
            .ok_or(KernelError::MissingOrdinal { ordinal })?;

        tracing::trace!(ordinal, name = export.name(), "dispatching kernel HLE export");
        Ok(export.call(context))
    }

    /// Returns one registered export.
    #[must_use]
    pub fn get(&self, ordinal: u16) -> Option<Arc<dyn KernelExport>> {
        self.exports.read().get(&ordinal).cloned()
    }

    /// Returns the number of registered exports.
    #[must_use]
    pub fn len(&self) -> usize {
        self.exports.read().len()
    }

    /// Returns true when no exports are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exports.read().is_empty()
    }
}

impl fmt::Debug for KernelRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let exports = self.exports.read();
        let mut ordinals = exports.keys().copied().collect::<Vec<_>>();
        ordinals.sort_unstable();
        formatter
            .debug_struct("KernelRegistry")
            .field("ordinals", &ordinals)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::CpuState;
    use exbawks_memory::SoftwareAddressSpace;

    use crate::StubExport;

    use super::*;

    #[test]
    fn registry_dispatches_a_stub() {
        let registry = KernelRegistry::new();
        registry
            .register(StubExport::new(7, "Example"))
            .expect("registration must succeed");

        let memory = SoftwareAddressSpace::new(4096).expect("memory must initialize");
        let mut cpu = CpuState::default();
        let mut context = KernelCallContext { cpu: &mut cpu, memory: &memory };
        let status = registry.dispatch(7, &mut context).expect("dispatch must succeed");

        assert_eq!(status, KernelStatus::NOT_IMPLEMENTED);
        assert_eq!(registry.get(7).expect("export must exist").name(), "Example");
    }

    #[test]
    fn registry_rejects_duplicate_ordinals() {
        let registry = KernelRegistry::new();
        registry
            .register(StubExport::new(7, "First"))
            .expect("first registration must succeed");

        assert_eq!(
            registry.register(StubExport::new(7, "Second")),
            Err(KernelError::DuplicateOrdinal { ordinal: 7 })
        );
    }
}
