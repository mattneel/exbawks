use exbawks_memory::GuestMemory;
use exbawks_types::GuestVa;
use serde::{Deserialize, Serialize};

use crate::CoreError;

const ORDINAL_BIT: u32 = 0x8000_0000;

/// One kernel import thunk entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelThunk {
    /// The guest address of the thunk slot.
    pub slot: GuestVa,
    /// The imported Xbox kernel ordinal.
    pub ordinal: u16,
}

/// A terminated list of kernel import thunks.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelThunkTable {
    /// Parsed thunk entries.
    pub entries: Vec<KernelThunk>,
}

impl KernelThunkTable {
    /// Reads an ordinal thunk table from checked guest memory.
    pub fn read(
        memory: &dyn GuestMemory,
        start: GuestVa,
        limit: usize,
    ) -> Result<Self, CoreError> {
        if limit == 0 {
            return Err(CoreError::InvalidConfiguration(
                "max_kernel_thunks must not be zero",
            ));
        }

        let mut entries = Vec::new();
        let mut slot = start;
        for _ in 0..limit {
            let value = memory.read_u32(slot)?;
            if value == 0 {
                return Ok(Self { entries });
            }
            if value & ORDINAL_BIT == 0 || value & 0x7FFF_0000 != 0 {
                return Err(CoreError::InvalidKernelThunk { address: slot, value });
            }

            entries.push(KernelThunk { slot, ordinal: value as u16 });
            slot = slot
                .checked_add(4)
                .ok_or(CoreError::KernelThunkAddressOverflow { address: slot })?;
        }

        Err(CoreError::KernelThunkLimit { limit })
    }
}

#[cfg(test)]
mod tests {
    use exbawks_memory::SoftwareAddressSpace;
    use exbawks_types::{GuestRange, MemoryPermissions};

    use super::*;

    #[test]
    fn thunk_reader_stops_at_zero() {
        let memory = SoftwareAddressSpace::new(4096).expect("memory must initialize");
        memory
            .map_anonymous(
                GuestRange::page_aligned(GuestVa(0x1000), 4096).expect("range must be valid"),
                MemoryPermissions::READ | MemoryPermissions::WRITE,
            )
            .expect("mapping must succeed");
        memory
            .write(GuestVa(0x1000), &[1, 0, 0, 0x80, 2, 0, 0, 0x80, 0, 0, 0, 0])
            .expect("write must succeed");

        let table = KernelThunkTable::read(&memory, GuestVa(0x1000), 8)
            .expect("table must parse");
        assert_eq!(table.entries.len(), 2);
        assert_eq!(table.entries[1].ordinal, 2);
    }
}
