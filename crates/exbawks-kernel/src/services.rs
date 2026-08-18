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

/// A request to open a guest file through a host-backed device (ADR 0014,
/// ADR 0016).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOpenRequest {
    /// The raw NT object path the guest supplied (device-qualified).
    pub path: String,
    /// Whether the guest requested any write or create-modifying access.
    pub write_access: bool,
    /// Whether the disposition creates the object when it is missing.
    pub create: bool,
    /// Whether the open names a directory (`FILE_DIRECTORY_FILE`).
    pub directory: bool,
}

/// The result of opening one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileOpened {
    /// The guest handle naming the open file object.
    pub handle: u32,
    /// True when the open created a new file, false when it opened an
    /// existing one (the `IoStatusBlock.Information` result).
    pub created: bool,
}

/// Size and position facts about one open file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileInfo {
    /// The file size in bytes.
    pub size: u64,
    /// The current byte offset of the file pointer.
    pub position: u64,
    /// True when the handle names a directory or device object.
    pub directory: bool,
}

/// One interrupt object's service routine, as the guest described it.
///
/// A title connects an interrupt and then waits: without the routine
/// recorded here there is nothing for a device to call, which is why its
/// USB stack initialises once and stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptRoutine {
    /// The guest `KINTERRUPT` object the routine belongs to.
    pub object: u32,
    /// The service routine's guest address.
    pub routine: u32,
    /// The context pointer the routine is called with.
    pub context: u32,
    /// The interrupt vector it is connected to.
    pub vector: u32,
}

/// The scanned-out display mode a title programmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayMode {
    /// The frame buffer's guest **physical** address (what the encoder
    /// scans out of).
    pub frame_buffer: u32,
    /// The Xbox surface format code (`0x12` is linear A8R8G8B8).
    pub format: u32,
    /// The distance between scanlines in bytes.
    pub pitch: u32,
    /// The mode word the title selected.
    pub mode: u32,
}

/// The disposition of a wait request (ADR 0017).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The object is already signaled (an auto-reset event is consumed).
    Signaled,
    /// The wait parks the calling thread until the object signals.
    Pending,
    /// No other thread is runnable to ever signal the object; the wait
    /// reports a timeout instead of deadlocking the guest.
    TimedOut,
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
    /// The named object was not found.
    #[error("the named object was not found")]
    NotFound,
    /// The request was denied (e.g. a write to a read-only device).
    #[error("the request was denied")]
    AccessDenied,
    /// A handle value does not name an open object.
    #[error("the handle does not name an open object")]
    InvalidHandle,
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

    /// Opens a file on a host-backed device (ADR 0014).
    ///
    /// Resolves the guest NT path within a mount, confined to the mount
    /// root, and returns the guest handle. The default implementation reports
    /// `Unsupported` for contexts without a device.
    fn open_file(&mut self, _request: FileOpenRequest) -> Result<FileOpened, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Reads bytes from one open file.
    ///
    /// `offset` reads at an explicit byte offset; `None` reads at the
    /// maintained file pointer and advances it. Returns fewer bytes than
    /// requested at end of file.
    fn read_file(
        &mut self,
        _handle: u32,
        _offset: Option<u64>,
        _len: u32,
    ) -> Result<Vec<u8>, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Returns the size and current position of one open file.
    fn file_info(&mut self, _handle: u32) -> Result<FileInfo, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Writes bytes to one open file on a writable mount (ADR 0016).
    ///
    /// `offset` writes at an explicit byte offset; `None` writes at the
    /// maintained file pointer and advances it. Returns the bytes written.
    fn write_file(
        &mut self,
        _handle: u32,
        _offset: Option<u64>,
        _bytes: &[u8],
    ) -> Result<u32, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Moves one open file's pointer to an absolute byte offset.
    fn set_file_position(&mut self, _handle: u32, _offset: u64) -> Result<(), KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Sets one open file's length (truncating or extending).
    fn set_file_length(&mut self, _handle: u32, _length: u64) -> Result<(), KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Allocates a physically contiguous, page-rounded guest buffer.
    ///
    /// Returns the guest address of the buffer in the kernel window (ADR
    /// 0010). Used by the `Mm*` contiguous family for GPU and DMA buffers.
    fn allocate_contiguous(&mut self, _bytes: u32) -> Result<GuestVa, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Claims the GPU instance-memory region (ADR 0013 exit economics).
    ///
    /// Like [`KernelServices::allocate_contiguous`], but the emulator also
    /// records the region so the hypervisor tier can alias it at the NV2A
    /// `PRAMIN` window instead of trapping every access.
    fn claim_gpu_instance(&mut self, bytes: u32) -> Result<GuestVa, KernelServiceError> {
        self.allocate_contiguous(bytes)
    }

    /// Returns the byte size of one pool/contiguous allocation.
    fn pool_block_size(&mut self, _address: u32) -> Result<u32, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Records that a guest region should survive a soft reboot (ADR 0015).
    ///
    /// `MmPersistContiguousMemory` marks the launch-data page persistent
    /// before a title relaunches itself; the emulator preserves the recorded
    /// regions across the reset. The default is a no-op.
    fn persist_memory(&mut self, _base: u32, _size: u32) {}

    /// Creates an object-namespace symbolic link (drive-letter mounting).
    ///
    /// Titles link `\??\D:` and their data letters to device paths at
    /// startup; the file device consults the links during path resolution.
    fn create_symbolic_link(
        &mut self,
        _name: String,
        _target: String,
    ) -> Result<(), KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Removes an object-namespace symbolic link.
    ///
    /// Returns `true` when the link existed.
    fn delete_symbolic_link(&mut self, _name: &str) -> bool {
        false
    }

    /// Creates an event object, returning its guest handle.
    fn create_event(
        &mut self,
        _manual_reset: bool,
        _initially_signaled: bool,
    ) -> Result<u32, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Signals an event, returning its previous signaled state.
    fn set_event(&mut self, _handle: u32) -> Result<bool, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Begins a wait on one handle (event or thread).
    fn wait_for_object(&mut self, _handle: u32) -> Result<WaitOutcome, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Records the display mode a title programmed on the video encoder.
    fn set_display_mode(&mut self, _mode: DisplayMode) {}

    /// Records an interrupt object's service routine, so the runtime can
    /// call it when a modelled device raises that vector.
    fn set_interrupt_routine(&mut self, _interrupt: InterruptRoutine) {}

    /// Marks an interrupt object connected or disconnected. A routine is
    /// only called while it is connected.
    fn connect_interrupt(&mut self, _object: u32, _connected: bool) {}

    /// Queues a deferred procedure call, reporting whether it was not
    /// already queued.
    fn queue_dpc(&mut self, _dpc: u32) -> bool {
        false
    }

    /// Resumes a suspended thread, returning its previous suspend count.
    fn resume_thread(&mut self, _handle: u32) -> Result<u32, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Suspends a thread, returning its previous suspend count.
    fn suspend_thread(&mut self, _handle: u32) -> Result<u32, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Creates a mutant object, returning its guest handle.
    fn create_mutant(&mut self, _initially_owned: bool) -> Result<u32, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Releases one level of a mutant's ownership, returning the previous
    /// recursion count. Reports `AccessDenied` when the calling thread does
    /// not own it.
    fn release_mutant(&mut self, _handle: u32) -> Result<u32, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Begins a wait on one dispatcher object named by guest address.
    ///
    /// The object's signal state lives in guest memory and the export
    /// checks it first; this only decides how an unsignaled wait resolves.
    fn wait_for_dispatcher_object(
        &mut self,
        _address: u32,
    ) -> Result<WaitOutcome, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Wakes every thread parked on a dispatcher object's guest address.
    fn signal_dispatcher_object(&mut self, _address: u32) {}

    /// Opens a handle to an existing symbolic-link object.
    fn open_symbolic_link(&mut self, _name: &str) -> Result<u32, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }

    /// Returns the target string of an open symbolic-link handle.
    fn query_symbolic_link(&mut self, _handle: u32) -> Result<String, KernelServiceError> {
        Err(KernelServiceError::Unsupported)
    }
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
