//! Nt* virtual-memory exports (HLE-003).
//!
//! Titles allocate their heaps and large buffers through
//! `NtAllocateVirtualMemory`. The export decodes the guest's in/out pointer
//! arguments and delegates the placement to the emulator's memory service
//! (ADR 0012); the service owns the user-range allocator and the map into
//! guest physical RAM. Reserve versus commit is honored (a reserve leaves
//! the range unbacked); the real reserve/commit region map is later work
//! (MEM-007).

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
use crate::{
    KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelServiceError, KernelStatus,
    VirtualAllocRequest,
};

/// Registers the Nt* virtual-memory exports.
pub(crate) fn register_vm_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(NtAllocateVirtualMemory)?;
    Ok(())
}

/// Reserves and/or commits guest virtual memory.
///
/// `NtAllocateVirtualMemory(BaseAddress, ZeroBits, RegionSize,
/// AllocationType, Protect)` reads the requested base and size through their
/// in/out pointers, asks the memory service to place the region, and writes
/// the chosen base and rounded size back through the same pointers.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtAllocateVirtualMemory;

impl KernelExport for NtAllocateVirtualMemory {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_ALLOCATE_VIRTUAL_MEMORY
    }

    fn name(&self) -> &'static str {
        "NtAllocateVirtualMemory"
    }

    fn stack_bytes(&self) -> u16 {
        20
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let (Some(base_ptr), Some(size_ptr), Some(allocation_type), Some(protect)) = (
            stack_argument(context, 0),
            stack_argument(context, 2),
            stack_argument(context, 3),
            stack_argument(context, 4),
        ) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        if base_ptr == 0 || size_ptr == 0 {
            return KernelStatus::INVALID_PARAMETER;
        }

        // BaseAddress and RegionSize are in/out pointers.
        let (Ok(base), Ok(size)) = (
            context.memory.read_u32(GuestVa(base_ptr)),
            context.memory.read_u32(GuestVa(size_ptr)),
        ) else {
            return KernelStatus::ACCESS_VIOLATION;
        };
        if size == 0 {
            return KernelStatus::INVALID_PARAMETER;
        }

        let request = VirtualAllocRequest { base, size, allocation_type, protect };
        match context.services.allocate_virtual_memory(request) {
            Ok(allocation) => {
                let _ = context.memory.write_u32(GuestVa(base_ptr), allocation.base.0);
                let _ = context.memory.write_u32(GuestVa(size_ptr), allocation.size);
                KernelStatus::SUCCESS
            }
            Err(KernelServiceError::ResourceExhausted) => KernelStatus::NO_MEMORY,
            // The allocator only ever reports exhaustion or an unsupported
            // context; the file-oriented errors cannot arise here.
            Err(_) => KernelStatus::INSUFFICIENT_RESOURCES,
        }
    }
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::CpuState;
    use exbawks_memory::{GuestMemory, SoftwareAddressSpace};
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, MemoryPermissions};

    use crate::UnsupportedServices;

    use super::*;

    /// Lays out a call frame with the five arguments above the return slot
    /// and runs the export against the given services.
    fn call(
        base_ptr: u32,
        size_ptr: u32,
        services: &mut dyn crate::KernelServices,
        memory: &SoftwareAddressSpace,
    ) -> KernelStatus {
        let args = [base_ptr, 0, size_ptr, 0x0000_1000, 0x0000_0004];
        for (slot, value) in args.iter().enumerate() {
            memory.write_u32(GuestVa(0x2004 + slot as u32 * 4), *value).expect("write");
        }
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut context = KernelCallContext { cpu: &mut cpu, memory, services, stop_request: None };
        NtAllocateVirtualMemory.call(&mut context)
    }

    fn mapped_memory() -> SoftwareAddressSpace {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 4 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        memory
    }

    #[test]
    fn a_null_pointer_argument_is_rejected() {
        let memory = mapped_memory();
        let mut services = UnsupportedServices;
        assert_eq!(
            call(0, 0x3000, &mut services, &memory),
            KernelStatus::INVALID_PARAMETER,
            "a null BaseAddress pointer is invalid"
        );
    }

    #[test]
    fn a_zero_region_size_is_rejected() {
        let memory = mapped_memory();
        // *RegionSize (at 0x3010) is zero.
        memory.write_u32(GuestVa(0x3010), 0).expect("write");
        let mut services = UnsupportedServices;
        assert_eq!(call(0x3000, 0x3010, &mut services, &memory), KernelStatus::INVALID_PARAMETER);
    }

    #[test]
    fn a_context_without_a_memory_service_reports_resources() {
        let memory = mapped_memory();
        // *RegionSize is nonzero so the request reaches the service.
        memory.write_u32(GuestVa(0x3010), 0x1000).expect("write");
        let mut services = UnsupportedServices;
        assert_eq!(
            call(0x3000, 0x3010, &mut services, &memory),
            KernelStatus::INSUFFICIENT_RESOURCES
        );
    }
}
