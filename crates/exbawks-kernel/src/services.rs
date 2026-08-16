use exbawks_types::GuestVa;
use thiserror::Error;

/// A request to create one guest thread (ADR 0011, ADR 0012).
///
/// Field meanings follow `PsCreateSystemThreadEx`; sizes are byte counts the
/// implementation rounds to pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadCreateRequest {
    /// Extension bytes reserved alongside the thread object.
    pub thread_extension_size: u32,
    /// The requested stack size in bytes.
    pub kernel_stack_size: u32,
    /// The thread-local-storage block size in bytes.
    pub tls_data_size: u32,
    /// The guest routine the thread starts at.
    pub start_routine: GuestVa,
    /// The first start-routine argument.
    pub start_context1: u32,
    /// The second start-routine argument.
    pub start_context2: u32,
    /// Whether the thread starts suspended.
    pub create_suspended: bool,
}

/// The guest-visible identity of one created thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadCreated {
    /// The guest handle value.
    pub handle: u32,
    /// The thread identifier.
    pub thread_id: u32,
    /// The guest address of the synthetic KTHREAD block.
    pub kthread: GuestVa,
}

/// A request to reserve and/or commit guest virtual memory.
///
/// Field meanings follow `NtAllocateVirtualMemory`. The raw Win32
/// `AllocationType` and `Protect` flags travel unchanged so the emulator
/// side owns their mapping to guest page permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualAllocRequest {
    /// The requested base address, or zero for kernel-chosen placement.
    pub base: u32,
    /// The requested region size in bytes.
    pub size: u32,
    /// The Win32 `MEM_*` allocation-type flags.
    pub allocation_type: u32,
    /// The Win32 `PAGE_*` protection flags.
    pub protect: u32,
}

/// The placement one allocation received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualAllocation {
    /// The page-aligned base the allocation received.
    pub base: GuestVa,
    /// The page-rounded region size in bytes.
    pub size: u32,
}

/// A kernel service failure.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum KernelServiceError {
    /// The running context provides no implementation of this service.
    #[error("the running context does not provide this kernel service")]
    Unsupported,
    /// Guest resources were exhausted.
    #[error("guest resources are exhausted")]
    ResourceExhausted,
}

/// Emulator-provided services kernel exports call (ADR 0012).
///
/// Methods are narrow, typed request/response operations. Implementations
/// must not switch guest threads directly; scheduling effects are recorded
/// as pending actions the run loop applies after the export returns
/// (ADR 0011).
pub trait KernelServices {
    /// Creates one guest thread and returns its identity.
    fn create_thread(
        &mut self,
        request: ThreadCreateRequest,
    ) -> Result<ThreadCreated, KernelServiceError>;

    /// Records the pending termination of the calling thread.
    fn exit_current_thread(&mut self, status: u32);

    /// Closes one guest handle.
    ///
    /// Returns `true` when the handle was open, `false` for an unknown
    /// handle so the caller can report `STATUS_INVALID_HANDLE`.
    fn close_handle(&mut self, handle: u32) -> bool;

    /// Reserves and/or commits a guest virtual-memory region.
    ///
    /// Returns the placement the request received. Commit maps physical
    /// pages; a reserve-only request records the address range without
    /// backing it. `ResourceExhausted` reports either address-space or
    /// physical-memory exhaustion.
    fn allocate_virtual_memory(
        &mut self,
        request: VirtualAllocRequest,
    ) -> Result<VirtualAllocation, KernelServiceError>;
}

/// A services implementation for contexts without an emulator.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedServices;

impl KernelServices for UnsupportedServices {
    fn create_thread(
        &mut self,
        _request: ThreadCreateRequest,
    ) -> Result<ThreadCreated, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    fn exit_current_thread(&mut self, _status: u32) {}

    fn close_handle(&mut self, _handle: u32) -> bool {
        false
    }

    fn allocate_virtual_memory(
        &mut self,
        _request: VirtualAllocRequest,
    ) -> Result<VirtualAllocation, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }
}
