//! One WHP partition running one 32-bit virtual processor (ADR 0013).
//!
//! The machine owns the partition and its single vCPU (the cooperative
//! scheduler multiplexes guest threads onto one processor, ADR 0011).
//! Bring-up follows the strict order from `docs/whp-notes.md`: create →
//! properties → setup → map/create-vcpu. The guest boots directly in flat
//! 32-bit protected mode (VT-x unrestricted guest); the kernel gate region
//! stays unmapped so a gate call exits with the ordinal encoded in the GPA.

use core::ffi::c_void;

use crate::api::{self, WhpApi, WhpError, check};

/// `WHvPartitionPropertyCodeProcessorCount`.
const PROPERTY_PROCESSOR_COUNT: u32 = 0x0000_1FFF;
/// `WHvPartitionPropertyCodeExtendedVmExits`.
const PROPERTY_EXTENDED_VM_EXITS: u32 = 0x0000_0001;
/// `WHvPartitionPropertyCodeExceptionExitBitmap`.
const PROPERTY_EXCEPTION_EXIT_BITMAP: u32 = 0x0000_0002;
/// `WHV_EXTENDED_VM_EXITS.ExceptionExit` (bit 2, per WinHvPlatformDefs.h).
const EXTENDED_EXIT_EXCEPTION: u64 = 1 << 2;
/// Intercepted exception vectors: #DE, #UD, #DF, #NP, #SS, #GP, #PF, #MF,
/// #AC, #XM — the faults a 32-bit guest can hit that we must diagnose.
const EXCEPTION_EXIT_BITMAP: u64 = (1 << 0)
    | (1 << 6)
    | (1 << 8)
    | (1 << 11)
    | (1 << 12)
    | (1 << 13)
    | (1 << 14)
    | (1 << 16)
    | (1 << 17)
    | (1 << 19);
/// The full `WHV_PARTITION_PROPERTY` union size the platform expects.
const PARTITION_PROPERTY_BYTES: usize = 32;

/// `WHvMapGpaRangeFlag*` map permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapFlags(pub u32);

impl MapFlags {
    /// Read access.
    pub const READ: Self = Self(1);
    /// Read and write access.
    pub const READ_WRITE: Self = Self(1 | 2);
    /// Read, write, and execute access.
    pub const READ_WRITE_EXECUTE: Self = Self(1 | 2 | 4);
}

/// `WHV_REGISTER_NAME` values used by the tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Register {
    Rax = 0x0000_0000,
    Rcx = 0x0000_0001,
    Rdx = 0x0000_0002,
    Rbx = 0x0000_0003,
    Rsp = 0x0000_0004,
    Rbp = 0x0000_0005,
    Rsi = 0x0000_0006,
    Rdi = 0x0000_0007,
    Rip = 0x0000_0010,
    Rflags = 0x0000_0011,
    Es = 0x0000_0012,
    Cs = 0x0000_0013,
    Ss = 0x0000_0014,
    Ds = 0x0000_0015,
    Fs = 0x0000_0016,
    Gs = 0x0000_0017,
    Ldtr = 0x0000_0018,
    Tr = 0x0000_0019,
    Idtr = 0x0000_001A,
    Gdtr = 0x0000_001B,
    Cr0 = 0x0000_001C,
    Cr2 = 0x0000_001D,
    Cr3 = 0x0000_001E,
    Cr4 = 0x0000_001F,
    Cr8 = 0x0000_0020,
    Efer = 0x0000_2001,
}

/// One 16-byte `WHV_REGISTER_VALUE`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C, align(16))]
pub struct RegisterValue {
    /// The low quadword (`Reg64` for scalar registers).
    pub low: u64,
    /// The high quadword.
    pub high: u64,
}

impl RegisterValue {
    /// A scalar 64-bit register value.
    #[must_use]
    pub const fn scalar(value: u64) -> Self {
        Self { low: value, high: 0 }
    }

    /// A `WHV_X64_SEGMENT_REGISTER` value: base, limit, selector, and the
    /// packed attribute word (type, S, DPL, P at bits 0..8; AVL, L, D/B, G at
    /// bits 12..16).
    #[must_use]
    pub const fn segment(base: u64, limit: u32, selector: u16, attributes: u16) -> Self {
        Self {
            low: base,
            high: (limit as u64) | ((selector as u64) << 32) | ((attributes as u64) << 48),
        }
    }
}

/// `WHV_VP_EXIT_CONTEXT`: the per-exit processor snapshot.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct VpExitContext {
    /// `WHV_X64_VP_EXECUTION_STATE`.
    pub execution_state: u16,
    /// Instruction length (low nibble) and CR8 (high nibble).
    pub instruction_length_cr8: u8,
    reserved: u8,
    reserved2: u32,
    /// The CS segment at exit (`WHV_X64_SEGMENT_REGISTER` layout).
    pub cs: [u64; 2],
    /// The instruction pointer at exit.
    pub rip: u64,
    /// The flags at exit.
    pub rflags: u64,
}

impl VpExitContext {
    /// The trapped instruction's byte length.
    #[must_use]
    pub const fn instruction_length(&self) -> u8 {
        self.instruction_length_cr8 & 0x0F
    }
}

/// `WHV_RUN_VP_EXIT_CONTEXT` (144 bytes: header, snapshot, reason payload).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct RawExitContext {
    exit_reason: u32,
    reserved: u32,
    vp: VpExitContext,
    payload: [u8; 96],
}

impl Default for RawExitContext {
    fn default() -> Self {
        Self { exit_reason: 0, reserved: 0, vp: VpExitContext::default(), payload: [0; 96] }
    }
}

/// One memory-access exit's decoded payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryAccess {
    /// The faulting guest physical address.
    pub gpa: u64,
    /// The faulting guest virtual address (valid when `gva_valid`).
    pub gva: u64,
    /// 0 = read, 1 = write, 2 = execute.
    pub access_type: u8,
    /// True when the GPA has no mapping (the gate/MMIO signal).
    pub gpa_unmapped: bool,
    /// True when `gva` is meaningful.
    pub gva_valid: bool,
    /// The captured instruction bytes (`len` of them).
    pub instruction_bytes: [u8; 16],
    /// How many instruction bytes were captured.
    pub instruction_byte_count: u8,
}

/// One intercepted guest exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestException {
    /// The exception vector (`#UD` = 6, `#GP` = 13, `#PF` = 14, …).
    pub vector: u8,
    /// The pushed error code, when the vector carries one.
    pub error_code: u32,
    /// The exception parameter (CR2 for a `#PF`).
    pub parameter: u64,
}

/// One port-I/O exit's decoded payload (non-string forms).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoPortAccess {
    /// The 16-bit port number.
    pub port: u16,
    /// True for `OUT`, false for `IN`.
    pub is_write: bool,
    /// The access width in bytes (1, 2, or 4).
    pub access_size: u8,
    /// True for the string/rep forms (`INS`/`OUTS`), which carry more state.
    pub string_op: bool,
    /// The guest RAX at exit (the OUT value source / IN destination).
    pub rax: u64,
}

/// One decoded virtual-processor exit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WhpExit {
    /// The guest executed `HLT`.
    Halt,
    /// The guest executed a port I/O instruction.
    IoPort(IoPortAccess),
    /// The guest touched an unmapped or protected guest physical address.
    MemoryAccess(MemoryAccess),
    /// The guest raised an intercepted exception.
    Exception(GuestException),
    /// The register state was rejected by the processor.
    InvalidRegisterValue,
    /// The guest reached an unrecoverable exception.
    UnrecoverableException,
    /// `WHvCancelRunVirtualProcessor` interrupted the run.
    Canceled,
    /// Any other `WHV_RUN_VP_EXIT_REASON`.
    Other(u32),
}

/// Exit reason codes (`WHV_RUN_VP_EXIT_REASON`).
mod exit_reason {
    pub const MEMORY_ACCESS: u32 = 0x0000_0001;
    pub const IO_PORT_ACCESS: u32 = 0x0000_0002;
    pub const UNRECOVERABLE_EXCEPTION: u32 = 0x0000_0004;
    pub const INVALID_VP_REGISTER_VALUE: u32 = 0x0000_0005;
    pub const X64_HALT: u32 = 0x0000_0008;
    pub const EXCEPTION: u32 = 0x0000_1002;
    pub const CANCELED: u32 = 0x0000_2001;
}

/// A page-aligned, zero-initialized host memory region backing guest RAM.
pub struct HostRegion {
    pointer: *mut u8,
    layout: std::alloc::Layout,
}

impl HostRegion {
    /// Allocates `bytes` of zeroed host memory, rounded up to whole 4 KiB
    /// pages (the platform requires page-granular mapping sizes).
    pub fn new(bytes: usize) -> Result<Self, WhpError> {
        let rounded = bytes.max(1).div_ceil(4096).saturating_mul(4096);
        let layout = std::alloc::Layout::from_size_align(rounded, 4096)
            .map_err(|_| WhpError::Unavailable)?;
        // SAFETY: the layout has nonzero size and valid 4 KiB alignment; the
        // allocation is owned by this struct and freed with the same layout.
        let pointer = unsafe { std::alloc::alloc_zeroed(layout) };
        if pointer.is_null() {
            return Err(WhpError::Unavailable);
        }
        Ok(Self { pointer, layout })
    }

    /// The region as a mutable byte slice.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: `pointer` is a live allocation of `layout.size()` bytes
        // exclusively owned through `&mut self`.
        unsafe { core::slice::from_raw_parts_mut(self.pointer, self.layout.size()) }
    }

    fn as_ptr(&self) -> *mut c_void {
        self.pointer.cast()
    }

    /// The region length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.layout.size()
    }

    /// True when the region is empty (never: allocation is page-rounded).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Drop for HostRegion {
    fn drop(&mut self) {
        // SAFETY: `pointer` came from `alloc_zeroed` with exactly `layout`.
        unsafe { std::alloc::dealloc(self.pointer, self.layout) };
    }
}

/// One partition with one 32-bit virtual processor.
pub struct Machine {
    api: &'static WhpApi,
    partition: *mut c_void,
    vcpu_created: bool,
    exit: RawExitContext,
    /// Host regions mapped into the partition. The machine owns them so the
    /// hypervisor can never write into freed host memory: a region maps in
    /// by value and lives until the partition is torn down.
    regions: Vec<HostRegion>,
    /// The attached guest address space; holding the `Arc` keeps its
    /// physical buffer alive for every mapping created from it.
    memory: Option<std::sync::Arc<exbawks_memory::SoftwareAddressSpace>>,
    /// This machine's slot in the cross-thread cancel registry.
    cancel_id: u64,
}

/// The cross-thread cancel registry: machine id → live partition handle.
///
/// A [`Canceller`] resolves its machine through this table under the lock,
/// and `Machine::drop` removes itself before deleting the partition, so a
/// cancel can never touch a freed handle.
static CANCEL_REGISTRY: std::sync::Mutex<Option<std::collections::HashMap<u64, usize>>> =
    std::sync::Mutex::new(None);
static NEXT_CANCEL_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A cross-thread handle that interrupts a machine's running processor.
#[derive(Clone)]
pub struct Canceller {
    api: &'static WhpApi,
    machine: u64,
}

impl core::fmt::Debug for Canceller {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("Canceller").field("machine", &self.machine).finish()
    }
}

impl Canceller {
    /// Kicks the processor out of `run` with a `Canceled` exit.
    ///
    /// A no-op once the machine is gone; the registry lock excludes teardown
    /// racing the call.
    pub fn cancel(&self) {
        let registry = CANCEL_REGISTRY.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(partition) =
            registry.as_ref().and_then(|entries| entries.get(&self.machine)).copied()
        {
            let target = partition as *mut c_void;
            // SAFETY: the registry holds only live partition handles (drop
            // removes them under this same lock), and the platform documents
            // cancellation as callable from any thread.
            let _ = unsafe { (self.api.cancel_run_virtual_processor)(target, 0, 0) };
        }
    }
}

impl Machine {
    /// Creates and sets up a one-processor partition.
    pub fn new() -> Result<Self, WhpError> {
        let api = api::load()?;

        let mut partition: *mut c_void = core::ptr::null_mut();
        // SAFETY: `partition` is a live out-pointer; on success the handle is
        // owned by this struct and deleted in `Drop`.
        check("WHvCreatePartition", unsafe { (api.create_partition)(&raw mut partition) })?;
        let cancel_id = NEXT_CANCEL_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut machine = Self {
            api,
            partition,
            vcpu_created: false,
            exit: RawExitContext::default(),
            regions: Vec::new(),
            memory: None,
            cancel_id,
        };

        // ProcessorCount = 1, plus exception interception so guest faults
        // surface as decodable exits instead of undiagnosable triple faults
        // (docs/whp-notes.md).
        machine.set_partition_property(PROPERTY_PROCESSOR_COUNT, 1)?;
        machine.set_partition_property(PROPERTY_EXTENDED_VM_EXITS, EXTENDED_EXIT_EXCEPTION)?;
        machine.set_partition_property(PROPERTY_EXCEPTION_EXIT_BITMAP, EXCEPTION_EXIT_BITMAP)?;
        // SAFETY: the partition handle is live and configured.
        check("WHvSetupPartition", unsafe { (machine.api.setup_partition)(machine.partition) })?;
        // SAFETY: the partition is set up; vCPU 0 with no flags.
        check("WHvCreateVirtualProcessor", unsafe {
            (machine.api.create_virtual_processor)(machine.partition, 0, 0)
        })?;
        machine.vcpu_created = true;

        let mut registry =
            CANCEL_REGISTRY.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        registry
            .get_or_insert_with(std::collections::HashMap::new)
            .insert(cancel_id, machine.partition as usize);
        drop(registry);
        Ok(machine)
    }

    /// Sets one partition property, passed as the full property union.
    fn set_partition_property(&mut self, code: u32, value: u64) -> Result<(), WhpError> {
        let mut property = [0_u8; PARTITION_PROPERTY_BYTES];
        property[..8].copy_from_slice(&value.to_le_bytes());
        // SAFETY: the property buffer is live and its size is passed.
        check("WHvSetPartitionProperty", unsafe {
            (self.api.set_partition_property)(
                self.partition,
                code,
                property.as_ptr().cast(),
                PARTITION_PROPERTY_BYTES as u32,
            )
        })
    }

    /// Returns a cross-thread handle that can interrupt `run`.
    #[must_use]
    pub fn canceller(&self) -> Canceller {
        Canceller { api: self.api, machine: self.cancel_id }
    }

    /// Attaches the guest address space and maps one physical window.
    ///
    /// Maps `bytes` of the space's physical RAM, starting `physical_offset`
    /// bytes in, at guest physical `gpa`. Holding the `Arc` keeps the
    /// buffer alive for the partition's lifetime, and the buffer's base
    /// address is stable by the `SoftwareAddressSpace` contract, so the
    /// mapping is sound. All three quantities must be page-granular.
    pub fn map_address_space(
        &mut self,
        memory: &std::sync::Arc<exbawks_memory::SoftwareAddressSpace>,
        physical_offset: u64,
        gpa: u64,
        bytes: u64,
        flags: MapFlags,
    ) -> Result<(), WhpError> {
        let end = physical_offset.checked_add(bytes).ok_or(WhpError::Unavailable)?;
        if end > memory.physical_len() as u64
            || !physical_offset.is_multiple_of(4096)
            || !bytes.is_multiple_of(4096)
            || !gpa.is_multiple_of(4096)
            || bytes == 0
        {
            return Err(WhpError::Unavailable);
        }
        match &self.memory {
            Some(attached) if std::sync::Arc::ptr_eq(attached, memory) => {}
            Some(_) => return Err(WhpError::Unavailable),
            None => self.memory = Some(memory.clone()),
        }

        let host = memory.physical_base_ptr().wrapping_add(physical_offset as usize);
        // SAFETY: `host..host+bytes` lies inside the attached space's live,
        // page-aligned, address-stable physical buffer (bounds checked
        // above), which the held `Arc` keeps alive for the mapping's life.
        check("WHvMapGpaRange", unsafe {
            (self.api.map_gpa_range)(self.partition, host.cast_mut().cast(), gpa, bytes, flags.0)
        })
    }

    /// Maps a host region into guest physical space at `gpa`.
    ///
    /// The machine takes ownership of the region for the mapping's lifetime
    /// (the hypervisor writes into the host memory, so it must outlive the
    /// partition — fill the region before mapping it); mapping
    /// replaces any prior mapping of the covered pages.
    pub fn map_gpa(
        &mut self,
        region: HostRegion,
        gpa: u64,
        flags: MapFlags,
    ) -> Result<(), WhpError> {
        // SAFETY: `region` is a live page-aligned, page-granular allocation
        // and the partition handle is live; the machine then owns the region,
        // so the mapping never outlives the host memory backing it.
        check("WHvMapGpaRange", unsafe {
            (self.api.map_gpa_range)(
                self.partition,
                region.as_ptr(),
                gpa,
                region.len() as u64,
                flags.0,
            )
        })?;
        self.regions.push(region);
        Ok(())
    }

    /// Removes a guest physical mapping, restoring the fault-on-touch state.
    pub fn unmap_gpa(&mut self, gpa: u64, bytes: u64) -> Result<(), WhpError> {
        // SAFETY: the partition handle is live; the range identifies guest
        // pages, not host memory.
        check("WHvUnmapGpaRange", unsafe { (self.api.unmap_gpa_range)(self.partition, gpa, bytes) })
    }

    /// Writes one batch of virtual-processor registers.
    pub fn set_registers(&mut self, entries: &[(Register, RegisterValue)]) -> Result<(), WhpError> {
        let names: Vec<u32> = entries.iter().map(|(name, _)| *name as u32).collect();
        let values: Vec<RegisterValue> = entries.iter().map(|(_, value)| *value).collect();
        // SAFETY: the parallel name/value arrays are live with the passed
        // length, and each value is a 16-byte `WHV_REGISTER_VALUE`.
        check("WHvSetVirtualProcessorRegisters", unsafe {
            (self.api.set_virtual_processor_registers)(
                self.partition,
                0,
                names.as_ptr(),
                names.len() as u32,
                values.as_ptr().cast(),
            )
        })
    }

    /// Reads one batch of virtual-processor registers.
    pub fn get_registers(&mut self, names: &[Register]) -> Result<Vec<RegisterValue>, WhpError> {
        let raw_names: Vec<u32> = names.iter().map(|name| *name as u32).collect();
        let mut values = vec![RegisterValue::default(); names.len()];
        // SAFETY: the parallel arrays are live with the passed length.
        check("WHvGetVirtualProcessorRegisters", unsafe {
            (self.api.get_virtual_processor_registers)(
                self.partition,
                0,
                raw_names.as_ptr(),
                raw_names.len() as u32,
                values.as_mut_ptr().cast(),
            )
        })?;
        Ok(values)
    }

    /// Boots the processor in flat 32-bit protected mode (docs/whp-notes.md).
    ///
    /// Flat 4 GiB code and data segments, paging off (guest-linear equals
    /// guest-physical, matching the software MMU), `RFLAGS = 2`, and the
    /// entry point in `RIP`.
    pub fn set_boot_state_32(&mut self, entry: u32, stack_pointer: u32) -> Result<(), WhpError> {
        const CODE_ATTRIBUTES: u16 = 0xC09B;
        const DATA_ATTRIBUTES: u16 = 0xC093;
        // A present 32-bit busy TSS and a present LDT keep VT-x's guest-state
        // checks satisfied even though the guest never uses either.
        const TSS_ATTRIBUTES: u16 = 0x008B;
        const LDT_ATTRIBUTES: u16 = 0x0082;

        let code = RegisterValue::segment(0, 0xFFFF_FFFF, 0x08, CODE_ATTRIBUTES);
        let data = RegisterValue::segment(0, 0xFFFF_FFFF, 0x10, DATA_ATTRIBUTES);
        self.set_registers(&[
            (Register::Cs, code),
            (Register::Ds, data),
            (Register::Es, data),
            (Register::Ss, data),
            (Register::Fs, data),
            (Register::Gs, data),
            (Register::Tr, RegisterValue::segment(0, 0xFFFF, 0, TSS_ATTRIBUTES)),
            (Register::Ldtr, RegisterValue::segment(0, 0xFFFF, 0, LDT_ATTRIBUTES)),
            (Register::Gdtr, RegisterValue::default()),
            (Register::Idtr, RegisterValue::default()),
            // PE set, paging off; PAE off; not long mode. CR4 enables
            // OSFXSR/OSXMMEXCPT so SSE executes (the Xbox CPU runs with
            // both set; without them every SSE instruction raises #UD).
            (Register::Cr0, RegisterValue::scalar(0x0000_0001)),
            (Register::Cr3, RegisterValue::default()),
            (Register::Cr4, RegisterValue::scalar(0x0000_0600)),
            (Register::Efer, RegisterValue::default()),
            (Register::Rflags, RegisterValue::scalar(0x0000_0002)),
            (Register::Rip, RegisterValue::scalar(u64::from(entry))),
            (Register::Rsp, RegisterValue::scalar(u64::from(stack_pointer))),
        ])
    }

    /// Runs the processor until the next exit and decodes it.
    pub fn run(&mut self) -> Result<WhpExit, WhpError> {
        self.exit = RawExitContext::default();
        // SAFETY: the exit context is a live buffer of the passed size,
        // reused across runs per the platform guidance.
        check("WHvRunVirtualProcessor", unsafe {
            (self.api.run_virtual_processor)(
                self.partition,
                0,
                (&raw mut self.exit).cast(),
                core::mem::size_of::<RawExitContext>() as u32,
            )
        })?;

        Ok(match self.exit.exit_reason {
            exit_reason::X64_HALT => WhpExit::Halt,
            exit_reason::MEMORY_ACCESS => {
                WhpExit::MemoryAccess(decode_memory_access(&self.exit.payload))
            }
            exit_reason::IO_PORT_ACCESS => WhpExit::IoPort(decode_io_port(&self.exit.payload)),
            exit_reason::EXCEPTION => WhpExit::Exception(decode_exception(&self.exit.payload)),
            exit_reason::INVALID_VP_REGISTER_VALUE => WhpExit::InvalidRegisterValue,
            exit_reason::UNRECOVERABLE_EXCEPTION => WhpExit::UnrecoverableException,
            exit_reason::CANCELED => WhpExit::Canceled,
            other => WhpExit::Other(other),
        })
    }

    /// The processor snapshot from the most recent exit.
    #[must_use]
    pub const fn exit_context(&self) -> &VpExitContext {
        &self.exit.vp
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        // Leave the cancel registry first, under its lock, so no canceller
        // can reach the partition once teardown starts.
        let mut registry =
            CANCEL_REGISTRY.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entries) = registry.as_mut() {
            entries.remove(&self.cancel_id);
        }
        drop(registry);
        // SAFETY: the handles are live and owned; the vCPU is deleted before
        // its partition.
        unsafe {
            if self.vcpu_created {
                let _ = (self.api.delete_virtual_processor)(self.partition, 0);
            }
            let _ = (self.api.delete_partition)(self.partition);
        }
    }
}

/// Decodes a `WHV_X64_IO_PORT_ACCESS_CONTEXT` payload (per
/// WinHvPlatformDefs.h: access info at 20 — `IsWrite` bit 0, `AccessSize`
/// bits 1..4, `StringOp` bit 4 — port at 24, `Rax` at 32).
fn decode_io_port(payload: &[u8; 96]) -> IoPortAccess {
    let access_info = u32::from_le_bytes([payload[20], payload[21], payload[22], payload[23]]);
    let mut quad = [0_u8; 8];
    quad.copy_from_slice(&payload[32..40]);
    IoPortAccess {
        port: u16::from_le_bytes([payload[24], payload[25]]),
        is_write: access_info & 1 != 0,
        access_size: ((access_info >> 1) & 0x7) as u8,
        string_op: access_info & 0x10 != 0,
        rax: u64::from_le_bytes(quad),
    }
}

/// Decodes a `WHV_VP_EXCEPTION_CONTEXT` payload (per WinHvPlatformDefs.h:
/// instruction bytes at 4, `ExceptionType` at 24, `ErrorCode` at 28,
/// `ExceptionParameter` at 32).
fn decode_exception(payload: &[u8; 96]) -> GuestException {
    let mut quad = [0_u8; 8];
    quad.copy_from_slice(&payload[32..40]);
    GuestException {
        vector: payload[24],
        error_code: u32::from_le_bytes([payload[28], payload[29], payload[30], payload[31]]),
        parameter: u64::from_le_bytes(quad),
    }
}

/// Decodes a `WHV_MEMORY_ACCESS_CONTEXT` payload.
fn decode_memory_access(payload: &[u8; 96]) -> MemoryAccess {
    let mut instruction_bytes = [0_u8; 16];
    instruction_bytes.copy_from_slice(&payload[4..20]);
    let access_info = u32::from_le_bytes([payload[20], payload[21], payload[22], payload[23]]);
    let mut quad = [0_u8; 8];
    quad.copy_from_slice(&payload[24..32]);
    let gpa = u64::from_le_bytes(quad);
    quad.copy_from_slice(&payload[32..40]);
    let gva = u64::from_le_bytes(quad);

    MemoryAccess {
        gpa,
        gva,
        access_type: (access_info & 0x3) as u8,
        gpa_unmapped: access_info & 0x4 != 0,
        gva_valid: access_info & 0x8 != 0,
        instruction_bytes,
        instruction_byte_count: payload[0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the hardware tests across the workspace.
    fn hardware_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::hardware_serial_lock()
    }

    /// Skips hardware tests on hosts without a usable hypervisor.
    fn usable() -> bool {
        let available = crate::probe_whp().usable();
        if !available {
            eprintln!("skipping: the Windows Hypervisor Platform is not usable on this host");
        }
        available
    }

    #[test]
    fn exit_context_layout_matches_the_platform() {
        assert_eq!(core::mem::size_of::<RawExitContext>(), 144);
        assert_eq!(core::mem::size_of::<VpExitContext>(), 40);
        assert_eq!(core::mem::size_of::<RegisterValue>(), 16);
        assert_eq!(core::mem::align_of::<RegisterValue>(), 16);
    }

    #[test]
    fn hlt_boots_to_a_halt_exit() {
        let _hardware = hardware_lock();
        if !usable() {
            return;
        }
        let mut machine = Machine::new().expect("partition must set up");
        let mut ram = HostRegion::new(0x1_0000).expect("host region allocates");
        // 0x1000: hlt.
        ram.bytes_mut()[0x1000] = 0xF4;
        machine.map_gpa(ram, 0, MapFlags::READ_WRITE_EXECUTE).expect("RAM maps");
        machine.set_boot_state_32(0x1000, 0x8000).expect("boot state applies");

        let exit = machine.run().expect("the processor runs");

        assert_eq!(exit, WhpExit::Halt, "context: {:?}", machine.exit_context());
        // The platform completes the HLT before exiting, so the reported RIP
        // is the instruction after it.
        assert_eq!(machine.exit_context().rip, 0x1001, "RIP reports past the halt");
    }

    #[test]
    fn an_unmapped_gate_address_exits_with_the_ordinal_gpa() {
        let _hardware = hardware_lock();
        if !usable() {
            return;
        }
        let mut machine = Machine::new().expect("partition must set up");
        let mut ram = HostRegion::new(0x1_0000).expect("host region allocates");
        // 0x1000: mov eax, [0xFF800008]  (ordinal 2's gate slot); hlt.
        ram.bytes_mut()[0x1000..0x1006].copy_from_slice(&[0xA1, 0x08, 0x00, 0x80, 0xFF, 0xF4]);
        machine.map_gpa(ram, 0, MapFlags::READ_WRITE_EXECUTE).expect("RAM maps");
        machine.set_boot_state_32(0x1000, 0x8000).expect("boot state applies");

        let exit = machine.run().expect("the processor runs");

        let WhpExit::MemoryAccess(access) = exit else {
            panic!("expected a memory-access exit, got {exit:?} ({:?})", machine.exit_context());
        };
        assert_eq!(access.gpa, 0xFF80_0008, "the gate slot address reaches the host");
        assert!(access.gpa_unmapped, "the gate region is unmapped by design");
        assert_eq!(access.access_type, 0, "a data read of the gate slot");
        assert_eq!(machine.exit_context().rip, 0x1000, "RIP reports the trapped instruction");
    }

    #[test]
    fn unmapping_ram_faults_the_next_fetch() {
        let _hardware = hardware_lock();
        if !usable() {
            return;
        }
        let mut machine = Machine::new().expect("partition must set up");
        let mut ram = HostRegion::new(0x1_0000).expect("host region allocates");
        ram.bytes_mut()[0x1000] = 0xF4; // hlt
        let ram_len = ram.len() as u64;
        machine.map_gpa(ram, 0, MapFlags::READ_WRITE_EXECUTE).expect("RAM maps");
        machine.set_boot_state_32(0x1000, 0x8000).expect("boot state applies");
        machine.unmap_gpa(0, ram_len).expect("RAM unmaps");

        let exit = machine.run().expect("the processor runs");

        let WhpExit::MemoryAccess(access) = exit else {
            panic!("expected a fetch fault, got {exit:?}");
        };
        assert_eq!(access.gpa, 0x1000, "the code fetch faults at the entry");
        assert!(access.gpa_unmapped);
        assert_eq!(access.access_type, 2, "an instruction fetch");
    }

    #[test]
    fn registers_round_trip_through_the_processor() {
        let _hardware = hardware_lock();
        if !usable() {
            return;
        }
        let mut machine = Machine::new().expect("partition must set up");
        machine
            .set_registers(&[
                (Register::Rax, RegisterValue::scalar(0x1234_5678)),
                (Register::Rbx, RegisterValue::scalar(0x9ABC_DEF0)),
            ])
            .expect("registers set");
        let values =
            machine.get_registers(&[Register::Rax, Register::Rbx]).expect("registers read");
        assert_eq!(values[0].low, 0x1234_5678);
        assert_eq!(values[1].low, 0x9ABC_DEF0);
    }
}
