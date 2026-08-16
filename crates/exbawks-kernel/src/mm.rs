//! Mm* memory-manager exports (HLE-009).
//!
//! Titles allocate physically contiguous buffers for the GPU and DMA engines
//! through the `Mm*` contiguous family, then read back a buffer's physical
//! address to program the hardware. The allocations are served from the
//! kernel window (ADR 0010); `MmGetPhysicalAddress` is the window mask. There
//! is no real GPU yet (only the null backend), so the returned physical
//! address is never dereferenced by hardware.

use exbawks_types::{GUEST_PAGE_SIZE, GuestVa};

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
    registry.register(MmQueryStatistics)?;
    registry.register(MmQueryAllocationSize)?;
    registry.register(MmClaimGpuInstanceMemory)?;
    Ok(())
}

/// Claims the GPU instance-memory region, returning its base in EAX.
///
/// On hardware this carves the top of physical RAM for the NV2A's
/// instance area; here it is a contiguous kernel block like any other GPU
/// buffer. `0xFFFF_FFFF` requests the default-sized claim.
#[derive(Debug, Default, Clone, Copy)]
pub struct MmClaimGpuInstanceMemory;

impl KernelExport for MmClaimGpuInstanceMemory {
    fn ordinal(&self) -> u16 {
        crate::ordinal::MM_CLAIM_GPU_INSTANCE_MEMORY
    }

    fn name(&self) -> &'static str {
        "MmClaimGpuInstanceMemory"
    }

    fn stack_bytes(&self) -> u16 {
        8
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        /// The retail kernel's default instance-memory claim.
        const DEFAULT_CLAIM_BYTES: u32 = 0x0011_0000;

        let requested = stack_argument(context, 0).unwrap_or(0);
        let padding_out = stack_argument(context, 1).unwrap_or(0);
        let bytes = if requested == u32::MAX { DEFAULT_CLAIM_BYTES } else { requested };
        if padding_out != 0 {
            let _ = context.memory.write_u32(GuestVa(padding_out), 0);
        }
        match context.services.claim_gpu_instance(bytes.max(GUEST_PAGE_SIZE)) {
            Ok(address) => KernelStatus(address.0),
            Err(_) => KernelStatus(0),
        }
    }
}

/// Reports the byte size of one contiguous allocation in EAX.
#[derive(Debug, Default, Clone, Copy)]
pub struct MmQueryAllocationSize;

impl KernelExport for MmQueryAllocationSize {
    fn ordinal(&self) -> u16 {
        crate::ordinal::MM_QUERY_ALLOCATION_SIZE
    }

    fn name(&self) -> &'static str {
        "MmQueryAllocationSize"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let base = stack_argument(context, 0).unwrap_or(0);
        match context.services.pool_block_size(base) {
            Ok(size) => KernelStatus(size),
            Err(_) => KernelStatus(0),
        }
    }
}

/// Answers `MM_STATISTICS` queries with a retail memory profile.
///
/// The caller sets `Length`; every covered field after it is filled. The
/// numbers model a 64 MiB retail console with roughly half its pages free —
/// synthetic but plausible, so titles sizing caches proceed.
#[derive(Debug, Default, Clone, Copy)]
pub struct MmQueryStatistics;

impl KernelExport for MmQueryStatistics {
    fn ordinal(&self) -> u16 {
        crate::ordinal::MM_QUERY_STATISTICS
    }

    fn name(&self) -> &'static str {
        "MmQueryStatistics"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let Some(stats) = stack_argument(context, 0).filter(|pointer| *pointer != 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let Ok(length) = context.memory.read_u32(GuestVa(stats)) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        // MM_STATISTICS: Length, TotalPhysicalPages, AvailablePages,
        // VirtualMemoryBytesCommitted, VirtualMemoryBytesReserved,
        // CachePagesCommitted, PoolPagesCommitted, StackPagesCommitted,
        // ImagePagesCommitted.
        let fields: [u32; 8] = [
            0x4000,      // 64 MiB of physical pages
            0x2000,      // half free
            0x0100_0000, // 16 MiB committed
            0x0200_0000, // 32 MiB reserved
            0x100,       // cache pages
            0x100,       // pool pages
            0x40,        // stack pages
            0x800,       // image pages
        ];
        for (index, value) in fields.iter().enumerate() {
            let offset = 4 + index as u32 * 4;
            if u64::from(offset) + 4 <= u64::from(length) {
                let _ = context.memory.write_u32(GuestVa(stats + offset), *value);
            }
        }
        KernelStatus::SUCCESS
    }
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
