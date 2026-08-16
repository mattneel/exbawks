//! Mm* memory-manager exports (HLE-009).
//!
//! Titles allocate physically contiguous buffers for the GPU and DMA engines
//! through the `Mm*` contiguous family, then read back a buffer's physical
//! address to program the hardware. The allocations are served from the
//! kernel window (ADR 0010); `MmGetPhysicalAddress` is the window mask. There
//! is no real GPU yet (only the null backend), so the returned physical
//! address is never dereferenced by hardware.

use crate::startup::stack_argument;
use crate::{KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus};

/// The physical-address mask for a kernel-window virtual address (ADR 0010:
/// the cached window is `0x8000_0000 | PA`).
const PHYSICAL_ADDRESS_MASK: u32 = 0x1FFF_FFFF;

/// Registers the Mm* contiguous-memory exports.
pub(crate) fn register_mm_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(MmAllocateContiguousMemory)?;
    registry.register(MmAllocateContiguousMemoryEx)?;
    registry.register(MmFreeContiguousMemory)?;
    registry.register(MmGetPhysicalAddress)?;
    registry.register(MmPersistContiguousMemory)?;
    Ok(())
}

/// Allocates one contiguous buffer, returning its guest address (or NULL).
fn allocate(context: &mut KernelCallContext<'_>, bytes: u32) -> KernelStatus {
    match context.services.allocate_contiguous(bytes) {
        // The buffer address is the return value in EAX; the runtime places
        // the "status" there, so the pointer rides out as the status.
        Ok(address) => KernelStatus(address.0),
        // A failed allocation returns a NULL pointer, not an NT status.
        Err(_) => KernelStatus(0),
    }
}

/// Allocates a physically contiguous buffer.
#[derive(Debug, Default, Clone, Copy)]
pub struct MmAllocateContiguousMemory;

impl KernelExport for MmAllocateContiguousMemory {
    fn ordinal(&self) -> u16 {
        crate::ordinal::MM_ALLOCATE_CONTIGUOUS_MEMORY
    }

    fn name(&self) -> &'static str {
        "MmAllocateContiguousMemory"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let bytes = stack_argument(context, 0).unwrap_or(0);
        allocate(context, bytes)
    }
}

/// Allocates a physically contiguous buffer with placement constraints.
///
/// The address-range and alignment constraints are ignored: the kernel-window
/// allocator already returns page-aligned, contiguous buffers, which satisfies
/// the alignments titles request in practice.
#[derive(Debug, Default, Clone, Copy)]
pub struct MmAllocateContiguousMemoryEx;

impl KernelExport for MmAllocateContiguousMemoryEx {
    fn ordinal(&self) -> u16 {
        crate::ordinal::MM_ALLOCATE_CONTIGUOUS_MEMORY_EX
    }

    fn name(&self) -> &'static str {
        "MmAllocateContiguousMemoryEx"
    }

    fn stack_bytes(&self) -> u16 {
        20
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // MmAllocateContiguousMemoryEx(NumberOfBytes, LowestAcceptableAddress,
        //   HighestAcceptableAddress, Alignment, ProtectionType).
        let bytes = stack_argument(context, 0).unwrap_or(0);
        allocate(context, bytes)
    }
}

/// Frees a contiguous buffer.
///
/// The bump allocator cannot reclaim pages yet (MEM-006), so this is a no-op
/// that leaks the buffer; a title reaching a title screen allocates its GPU
/// buffers once, so the leak does not accumulate.
#[derive(Debug, Default, Clone, Copy)]
pub struct MmFreeContiguousMemory;

impl KernelExport for MmFreeContiguousMemory {
    fn ordinal(&self) -> u16 {
        crate::ordinal::MM_FREE_CONTIGUOUS_MEMORY
    }

    fn name(&self) -> &'static str {
        "MmFreeContiguousMemory"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, _context: &mut KernelCallContext<'_>) -> KernelStatus {
        // Returns VOID; the value in EAX is irrelevant to the caller.
        KernelStatus::SUCCESS
    }
}

/// Marks a contiguous region to survive a soft reboot (ADR 0015).
///
/// A title persists its launch-data page before relaunching itself; the
/// emulator preserves the recorded region across the reset. Records the
/// region through the memory service and reports success.
#[derive(Debug, Default, Clone, Copy)]
pub struct MmPersistContiguousMemory;

impl KernelExport for MmPersistContiguousMemory {
    fn ordinal(&self) -> u16 {
        crate::ordinal::MM_PERSIST_CONTIGUOUS_MEMORY
    }

    fn name(&self) -> &'static str {
        "MmPersistContiguousMemory"
    }

    fn stack_bytes(&self) -> u16 {
        12
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // MmPersistContiguousMemory(BaseAddress, NumberOfBytes, Persist).
        let base = stack_argument(context, 0).unwrap_or(0);
        let size = stack_argument(context, 1).unwrap_or(0);
        let persist = stack_argument(context, 2).unwrap_or(0);
        if base != 0 && persist != 0 {
            context.services.persist_memory(base, size);
        }
        KernelStatus::SUCCESS
    }
}

/// Returns the physical address backing a kernel-window virtual address.
#[derive(Debug, Default, Clone, Copy)]
pub struct MmGetPhysicalAddress;

impl KernelExport for MmGetPhysicalAddress {
    fn ordinal(&self) -> u16 {
        crate::ordinal::MM_GET_PHYSICAL_ADDRESS
    }

    fn name(&self) -> &'static str {
        "MmGetPhysicalAddress"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let address = stack_argument(context, 0).unwrap_or(0);
        // The physical address is the low bits of the window virtual address.
        KernelStatus(address & PHYSICAL_ADDRESS_MASK)
    }
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::CpuState;
    use exbawks_memory::{GuestMemory, SoftwareAddressSpace};
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, GuestVa, MemoryPermissions};

    use crate::{KernelServiceError, KernelServices, UnsupportedServices};

    use super::*;

    /// A service that hands out one fixed contiguous address.
    struct ContiguousFake(u32);

    impl KernelServices for ContiguousFake {
        fn create_thread(
            &mut self,
            _request: crate::ThreadCreateRequest,
        ) -> Result<crate::ThreadCreated, KernelServiceError> {
            Err(KernelServiceError::Unsupported)
        }

        fn exit_current_thread(&mut self, _status: u32) {}

        fn close_handle(&mut self, _handle: u32) -> bool {
            false
        }

        fn allocate_virtual_memory(
            &mut self,
            _request: crate::VirtualAllocRequest,
        ) -> Result<crate::VirtualAllocation, KernelServiceError> {
            Err(KernelServiceError::Unsupported)
        }

        fn allocate_contiguous(&mut self, _bytes: u32) -> Result<GuestVa, KernelServiceError> {
            Ok(GuestVa(self.0))
        }
    }

    fn memory() -> SoftwareAddressSpace {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        memory
    }

    fn call_one(
        export: &dyn KernelExport,
        argument: u32,
        services: &mut dyn KernelServices,
        memory: &SoftwareAddressSpace,
    ) -> KernelStatus {
        memory.write_u32(GuestVa(0x1004), argument).expect("write");
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x1000;
        let mut context = KernelCallContext { cpu: &mut cpu, memory, services, stop_request: None };
        export.call(&mut context)
    }

    #[test]
    fn allocate_returns_the_service_address() {
        let memory = memory();
        let mut services = ContiguousFake(0x8020_0000);
        let result = call_one(&MmAllocateContiguousMemory, 0x1000, &mut services, &memory);
        assert_eq!(result, KernelStatus(0x8020_0000));
    }

    #[test]
    fn allocate_returns_null_without_a_service() {
        let memory = memory();
        let mut services = UnsupportedServices;
        let result = call_one(&MmAllocateContiguousMemory, 0x1000, &mut services, &memory);
        assert_eq!(result, KernelStatus(0), "a failed allocation is NULL");
    }

    #[test]
    fn physical_address_masks_the_window() {
        let memory = memory();
        let mut services = UnsupportedServices;
        let result = call_one(&MmGetPhysicalAddress, 0x8020_1000, &mut services, &memory);
        assert_eq!(result, KernelStatus(0x0020_1000));
    }
}
