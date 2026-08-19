use exbawks_types::{GuestVa, StopReason};

use crate::{
    KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus, SuccessExport,
    ThreadCreateRequest,
};

/// Xbox kernel export ordinals from the public XboxDev export table.
pub mod ordinal {
    /// `AvGetSavedDataAddress`.
    pub const AV_GET_SAVED_DATA_ADDRESS: u16 = 1;
    /// `AvSendTVEncoderOption`.
    pub const AV_SEND_TV_ENCODER_OPTION: u16 = 2;
    /// `AvSetDisplayMode`.
    pub const AV_SET_DISPLAY_MODE: u16 = 3;
    /// `AvSetSavedDataAddress`.
    pub const AV_SET_SAVED_DATA_ADDRESS: u16 = 4;
    /// `DbgPrint`.
    pub const DBG_PRINT: u16 = 8;
    /// `ExAllocatePool`.
    pub const EX_ALLOCATE_POOL: u16 = 14;
    /// `ExAllocatePoolWithTag`.
    pub const EX_ALLOCATE_POOL_WITH_TAG: u16 = 15;
    /// `ExFreePool`.
    pub const EX_FREE_POOL: u16 = 17;
    /// `ExQueryPoolBlockSize`.
    pub const EX_QUERY_POOL_BLOCK_SIZE: u16 = 23;
    /// `ExQueryNonVolatileSetting`.
    pub const EX_QUERY_NON_VOLATILE_SETTING: u16 = 24;
    /// `HalGetInterruptVector`.
    pub const HAL_GET_INTERRUPT_VECTOR: u16 = 44;
    /// `HalReadWritePCISpace`.
    pub const HAL_READ_WRITE_PCI_SPACE: u16 = 46;
    /// `HalRegisterShutdownNotification`.
    pub const HAL_REGISTER_SHUTDOWN_NOTIFICATION: u16 = 47;
    /// `HalReturnToFirmware`.
    pub const HAL_RETURN_TO_FIRMWARE: u16 = 49;
    /// `IoCreateSymbolicLink`.
    pub const IO_CREATE_SYMBOLIC_LINK: u16 = 67;
    /// `IoDeleteSymbolicLink`.
    pub const IO_DELETE_SYMBOLIC_LINK: u16 = 69;
    /// `KeDelayExecutionThread`.
    pub const KE_DELAY_EXECUTION_THREAD: u16 = 99;
    /// `KeInitializeDpc`.
    pub const KE_CANCEL_TIMER: u16 = 97;
    pub const NT_YIELD_EXECUTION: u16 = 238;
    pub const OB_REFERENCE_OBJECT_BY_HANDLE: u16 = 246;
    pub const OBF_DEREFERENCE_OBJECT: u16 = 250;
    pub const KE_INITIALIZE_DPC: u16 = 107;
    pub const KE_INSERT_QUEUE_DPC: u16 = 119;
    /// `KeInitializeTimerEx`.
    pub const KE_INITIALIZE_TIMER_EX: u16 = 113;
    /// `KeConnectInterrupt`.
    pub const KE_CONNECT_INTERRUPT: u16 = 98;
    /// `KeDisconnectInterrupt`.
    pub const KE_DISCONNECT_INTERRUPT: u16 = 100;
    /// `KeGetCurrentIrql`.
    pub const KE_GET_CURRENT_IRQL: u16 = 103;
    /// `KeInitializeInterrupt`.
    pub const KE_INITIALIZE_INTERRUPT: u16 = 109;
    /// `KeQuerySystemTime`.
    pub const KE_QUERY_SYSTEM_TIME: u16 = 128;
    /// `KeRaiseIrqlToDpcLevel`.
    pub const KE_RAISE_IRQL_TO_DPC_LEVEL: u16 = 129;
    /// `KeSetEvent`.
    pub const KE_SET_EVENT: u16 = 145;
    /// `KeStallExecutionProcessor`.
    pub const KE_STALL_EXECUTION_PROCESSOR: u16 = 151;
    /// `KeWaitForSingleObject`.
    pub const KE_WAIT_FOR_SINGLE_OBJECT: u16 = 159;
    /// `KfRaiseIrql`.
    pub const KF_RAISE_IRQL: u16 = 160;
    /// `KfLowerIrql`.
    pub const KF_LOWER_IRQL: u16 = 161;
    /// `KeSetTimer`.
    pub const KE_SET_TIMER: u16 = 149;
    /// `MmAllocateContiguousMemory`.
    pub const MM_ALLOCATE_CONTIGUOUS_MEMORY: u16 = 165;
    /// `MmAllocateContiguousMemoryEx`.
    pub const MM_ALLOCATE_CONTIGUOUS_MEMORY_EX: u16 = 166;
    /// `MmClaimGpuInstanceMemory`.
    pub const MM_CLAIM_GPU_INSTANCE_MEMORY: u16 = 168;
    /// `MmFreeContiguousMemory`.
    pub const MM_FREE_CONTIGUOUS_MEMORY: u16 = 171;
    /// `MmGetPhysicalAddress`.
    pub const MM_GET_PHYSICAL_ADDRESS: u16 = 173;
    /// `MmLockUnlockBufferPages`.
    pub const MM_LOCK_UNLOCK_BUFFER_PAGES: u16 = 175;
    /// `MmPersistContiguousMemory`.
    pub const MM_PERSIST_CONTIGUOUS_MEMORY: u16 = 178;
    /// `MmQueryAllocationSize`.
    pub const MM_QUERY_ALLOCATION_SIZE: u16 = 180;
    /// `MmQueryStatistics`.
    pub const MM_QUERY_STATISTICS: u16 = 181;
    /// `NtAllocateVirtualMemory`.
    pub const NT_ALLOCATE_VIRTUAL_MEMORY: u16 = 184;
    /// `NtClose`.
    pub const NT_CLOSE: u16 = 187;
    /// `NtCreateEvent`.
    pub const NT_CREATE_EVENT: u16 = 189;
    /// `NtCreateFile`.
    pub const NT_CREATE_FILE: u16 = 190;
    /// `NtCreateMutant`.
    pub const NT_CREATE_MUTANT: u16 = 192;
    /// `NtDeviceIoControlFile`.
    pub const NT_DEVICE_IO_CONTROL_FILE: u16 = 196;
    /// `NtFreeVirtualMemory`.
    pub const NT_FREE_VIRTUAL_MEMORY: u16 = 199;
    /// `NtOpenFile`.
    pub const NT_OPEN_FILE: u16 = 202;
    /// `NtOpenSymbolicLinkObject`.
    pub const NT_OPEN_SYMBOLIC_LINK_OBJECT: u16 = 203;
    /// `NtQuerySymbolicLinkObject`.
    pub const NT_QUERY_SYMBOLIC_LINK_OBJECT: u16 = 215;
    /// `NtQueryInformationFile`.
    pub const NT_QUERY_INFORMATION_FILE: u16 = 211;
    /// `NtQueryVolumeInformationFile`.
    pub const NT_QUERY_VOLUME_INFORMATION_FILE: u16 = 218;
    /// `NtReadFile`.
    pub const NT_READ_FILE: u16 = 219;
    /// `NtReleaseMutant`.
    pub const NT_RELEASE_MUTANT: u16 = 221;
    /// `NtResumeThread`.
    pub const NT_RESUME_THREAD: u16 = 224;
    /// `NtSetEvent`.
    pub const NT_SET_EVENT: u16 = 225;
    /// `NtSetInformationFile`.
    pub const NT_SET_INFORMATION_FILE: u16 = 226;
    /// `NtSuspendThread`.
    pub const NT_SUSPEND_THREAD: u16 = 231;
    /// `NtWaitForSingleObject`.
    pub const NT_WAIT_FOR_SINGLE_OBJECT: u16 = 233;
    /// `NtWaitForSingleObjectEx`.
    pub const NT_WAIT_FOR_SINGLE_OBJECT_EX: u16 = 234;
    /// `NtWaitForMultipleObjectsEx`.
    pub const NT_WAIT_FOR_MULTIPLE_OBJECTS_EX: u16 = 235;
    /// `NtWriteFile`.
    pub const NT_WRITE_FILE: u16 = 236;
    /// `PsCreateSystemThreadEx`.
    pub const PS_CREATE_SYSTEM_THREAD_EX: u16 = 255;
    /// `PsTerminateSystemThread`.
    pub const PS_TERMINATE_SYSTEM_THREAD: u16 = 258;
    /// `RtlEnterCriticalSection`.
    pub const RTL_ENTER_CRITICAL_SECTION: u16 = 277;
    /// `RtlEqualString`.
    pub const RTL_EQUAL_STRING: u16 = 279;
    /// `RtlInitAnsiString`.
    pub const RTL_INIT_ANSI_STRING: u16 = 289;
    /// `RtlInitializeCriticalSection`.
    pub const RTL_INITIALIZE_CRITICAL_SECTION: u16 = 291;
    /// `RtlLeaveCriticalSection`.
    pub const RTL_LEAVE_CRITICAL_SECTION: u16 = 294;
    /// `RtlNtStatusToDosError`.
    pub const RTL_NT_STATUS_TO_DOS_ERROR: u16 = 301;
    /// `XeLoadSection`.
    pub const XE_LOAD_SECTION: u16 = 327;
    /// `XeUnloadSection`.
    pub const XE_UNLOAD_SECTION: u16 = 328;
}

/// Benign exports that succeed as no-ops on the boot path:
/// (ordinal, name, stdcall argument bytes).
const BENIGN_SUCCESS: [(u16, &str, u16); 3] = [
    (ordinal::HAL_REGISTER_SHUTDOWN_NOTIFICATION, "HalRegisterShutdownNotification", 8),
    // A calibrated busy-wait; HLE time passes through the virtual clock.
    (ordinal::KE_STALL_EXECUTION_PROCESSOR, "KeStallExecutionProcessor", 4),
    // Page pinning for device DMA; HLE pages never move.
    (ordinal::MM_LOCK_UNLOCK_BUFFER_PAGES, "MmLockUnlockBufferPages", 12),
];

/// Registers the startup export set for one synthetic guest thread.
pub fn register_startup_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(DbgPrint)?;
    registry.register(HalReturnToFirmware)?;
    registry.register(PsCreateSystemThreadEx)?;
    registry.register(PsTerminateSystemThread)?;
    registry.register(NtClose)?;
    registry.register(NtCreateEvent)?;
    registry.register(NtSetEvent)?;
    registry.register(NtResumeThread)?;
    registry.register(NtSuspendThread)?;
    registry.register(NtYieldExecution)?;
    registry.register(ObReferenceObjectByHandle)?;
    registry.register(ObfDereferenceObject)?;
    registry.register(NtWaitForSingleObject)?;
    registry.register(NtWaitForSingleObjectEx)?;
    registry.register(NtWaitForMultipleObjectsEx)?;
    crate::rtl::register_rtl_exports(registry)?;
    crate::ke::register_ke_exports(registry)?;
    crate::ex::register_ex_exports(registry)?;
    crate::vm::register_vm_exports(registry)?;
    crate::file::register_file_exports(registry)?;
    crate::mm::register_mm_exports(registry)?;
    crate::io::register_io_exports(registry)?;
    crate::xe::register_xe_exports(registry)?;
    crate::irql::register_irql_exports(registry)?;
    crate::av::register_av_exports(registry)?;
    crate::dispatcher::register_dispatcher_exports(registry)?;
    crate::mutant::register_mutant_exports(registry)?;
    for (ordinal, name, stack_bytes) in BENIGN_SUCCESS {
        registry.register(SuccessExport::new(ordinal, name, stack_bytes))?;
    }
    registry.register(KeDelayExecutionThread)?;
    Ok(())
}

/// Parks the calling thread for an interval — a sleep (ADR 0021).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeDelayExecutionThread;

impl KernelExport for KeDelayExecutionThread {
    fn ordinal(&self) -> u16 {
        ordinal::KE_DELAY_EXECUTION_THREAD
    }

    fn name(&self) -> &'static str {
        "KeDelayExecutionThread"
    }

    fn stack_bytes(&self) -> u16 {
        12
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // KeDelayExecutionThread(WaitMode, Alertable, Interval). A null
        // interval pointer is an error; an absolute or zero interval has
        // already arrived and returns at once.
        let Some(pointer) = stack_argument(context, 2).filter(|pointer| *pointer != 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let (Ok(low), Ok(high)) = (
            context.memory.read_u32(GuestVa(pointer)),
            context.memory.read_u32(GuestVa(pointer.wrapping_add(4))),
        ) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let value = i64::from(high) << 32 | i64::from(low);
        let milliseconds = if value < 0 { (value.unsigned_abs() / 10_000).max(1) } else { 0 };
        match context.services.sleep_thread(milliseconds) {
            Ok(_) => KernelStatus::SUCCESS,
            Err(_) => KernelStatus::INVALID_PARAMETER,
        }
    }
}

/// Prints one guest ANSI string through host tracing.
///
/// The real export takes cdecl variable arguments; this implementation
/// prints the format string without substitution.
#[derive(Debug, Default, Clone, Copy)]
pub struct DbgPrint;

impl KernelExport for DbgPrint {
    fn ordinal(&self) -> u16 {
        ordinal::DBG_PRINT
    }

    fn name(&self) -> &'static str {
        "DbgPrint"
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let Some(pointer) = stack_argument(context, 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let Some(text) = read_guest_string(context, GuestVa(pointer)) else {
            return KernelStatus::INVALID_PARAMETER;
        };

        tracing::info!(target: "exbawks_guest", "{text}");
        KernelStatus::SUCCESS
    }
}

/// Ends emulation with a controlled guest exit.
#[derive(Debug, Default, Clone, Copy)]
pub struct HalReturnToFirmware;

impl KernelExport for HalReturnToFirmware {
    fn ordinal(&self) -> u16 {
        ordinal::HAL_RETURN_TO_FIRMWARE
    }

    fn name(&self) -> &'static str {
        "HalReturnToFirmware"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let routine = stack_argument(context, 0).unwrap_or(0);
        // ReturnFirmwareReboot (1) and ReturnFirmwareQuickReboot (2) ask for a
        // machine restart, which a title uses to relaunch itself (ADR 0015);
        // halt and fatal end the run.
        let stop = match routine {
            1 | 2 => StopReason::Reboot { routine },
            code => StopReason::GuestExit { code },
        };
        context.stop_request = Some(stop);
        KernelStatus::SUCCESS
    }
}

/// Creates one guest thread through the kernel services (ADR 0011/0012).
///
/// Arguments follow the public `PsCreateSystemThreadEx` shape: handle out,
/// extension size, stack size, TLS size, optional thread-id out, two start
/// contexts, create-suspended, debugger flag (ignored), start routine.
#[derive(Debug, Default, Clone, Copy)]
pub struct PsCreateSystemThreadEx;

impl KernelExport for PsCreateSystemThreadEx {
    fn ordinal(&self) -> u16 {
        ordinal::PS_CREATE_SYSTEM_THREAD_EX
    }

    fn name(&self) -> &'static str {
        "PsCreateSystemThreadEx"
    }

    fn stack_bytes(&self) -> u16 {
        40
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let mut arguments = [0_u32; 10];
        for (index, slot) in arguments.iter_mut().enumerate() {
            let Some(value) = stack_argument(context, index as u32) else {
                return KernelStatus::INVALID_PARAMETER;
            };
            *slot = value;
        }
        let [
            handle_out,
            extension,
            stack_size,
            tls_size,
            id_out,
            context1,
            context2,
            suspended,
            _debugger,
            routine,
        ] = arguments;

        // Probe the out-parameters before creating anything, so a bad
        // pointer fails without leaking a thread.
        if context.memory.write_u32(GuestVa(handle_out), 0).is_err() {
            return KernelStatus::INVALID_PARAMETER;
        }
        if id_out != 0 && context.memory.write_u32(GuestVa(id_out), 0).is_err() {
            return KernelStatus::INVALID_PARAMETER;
        }

        let request = ThreadCreateRequest {
            thread_extension_size: extension,
            kernel_stack_size: stack_size,
            tls_data_size: tls_size,
            start_routine: GuestVa(routine),
            start_context1: context1,
            start_context2: context2,
            create_suspended: suspended != 0,
        };
        let Ok(created) = context.services.create_thread(request) else {
            return KernelStatus::INSUFFICIENT_RESOURCES;
        };

        if context.memory.write_u32(GuestVa(handle_out), created.handle).is_err() {
            return KernelStatus::INVALID_PARAMETER;
        }
        if id_out != 0 && context.memory.write_u32(GuestVa(id_out), created.thread_id).is_err() {
            return KernelStatus::INVALID_PARAMETER;
        }
        KernelStatus::SUCCESS
    }
}

/// Terminates the calling guest thread (ADR 0011).
///
/// The termination is recorded as a pending action; the run loop performs
/// the switch after this call returns.
#[derive(Debug, Default, Clone, Copy)]
pub struct PsTerminateSystemThread;

impl KernelExport for PsTerminateSystemThread {
    fn ordinal(&self) -> u16 {
        ordinal::PS_TERMINATE_SYSTEM_THREAD
    }

    fn name(&self) -> &'static str {
        "PsTerminateSystemThread"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let status = stack_argument(context, 0).unwrap_or(0);
        context.services.exit_current_thread(status);
        KernelStatus::SUCCESS
    }
}

/// Resumes a suspended thread, reporting its previous suspend count.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtResumeThread;

impl KernelExport for NtResumeThread {
    fn ordinal(&self) -> u16 {
        ordinal::NT_RESUME_THREAD
    }

    fn name(&self) -> &'static str {
        "NtResumeThread"
    }

    fn stack_bytes(&self) -> u16 {
        8
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtResumeThread(ThreadHandle, PreviousSuspendCount).
        let Some(handle) = stack_argument(context, 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let previous_out = stack_argument(context, 1).unwrap_or(0);
        match context.services.resume_thread(handle) {
            Ok(previous) => {
                if previous_out != 0 {
                    let _ = context.memory.write_u32(GuestVa(previous_out), previous);
                }
                KernelStatus::SUCCESS
            }
            Err(_) => KernelStatus::INVALID_HANDLE,
        }
    }
}

/// Resolves a handle to the object it names.
///
/// A caller reads fields off what it is given — a thread's control block
/// has its own — so it is handed the structure the runtime built for that
/// handle. A handle with no body behind it is refused rather than answered
/// with itself, because a caller that dereferences the answer would fault
/// on an address that is a handle rather than a pointer.
#[derive(Debug, Default, Clone, Copy)]
pub struct ObReferenceObjectByHandle;

impl KernelExport for ObReferenceObjectByHandle {
    fn ordinal(&self) -> u16 {
        ordinal::OB_REFERENCE_OBJECT_BY_HANDLE
    }

    fn name(&self) -> &'static str {
        "ObReferenceObjectByHandle"
    }

    fn stack_bytes(&self) -> u16 {
        12
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // ObReferenceObjectByHandle(Handle, ObjectType, ReturnedObject).
        let Some(handle) = stack_argument(context, 0).filter(|handle| *handle != 0) else {
            return KernelStatus::INVALID_HANDLE;
        };
        let Some(object) = context.services.object_for_handle(handle) else {
            return KernelStatus::INVALID_HANDLE;
        };
        let object_out = stack_argument(context, 2).unwrap_or(0);
        if object_out != 0 && context.memory.write_u32(GuestVa(object_out), object).is_err() {
            return KernelStatus::INVALID_PARAMETER;
        }
        KernelStatus::SUCCESS
    }
}

/// Releases a reference taken on an object.
///
/// Nothing is counted, because nothing was allocated to count: the
/// reference above hands back the handle itself. It takes its object in a
/// register rather than on the stack, so it clears no stack of its own.
#[derive(Debug, Default, Clone, Copy)]
pub struct ObfDereferenceObject;

impl KernelExport for ObfDereferenceObject {
    fn ordinal(&self) -> u16 {
        ordinal::OBF_DEREFERENCE_OBJECT
    }

    fn name(&self) -> &'static str {
        "ObfDereferenceObject"
    }

    fn stack_bytes(&self) -> u16 {
        0
    }

    fn call(&self, _context: &mut KernelCallContext<'_>) -> KernelStatus {
        KernelStatus::SUCCESS
    }
}

/// Gives up the rest of the calling thread's turn.
///
/// A title calls this while it waits for work another thread is doing, so
/// a caller that is told nothing else was ready will call it again
/// immediately; under the cooperative scheduler (ADR 0011) that is a
/// yield that costs a turn rather than one that spins the processor.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtYieldExecution;

impl KernelExport for NtYieldExecution {
    fn ordinal(&self) -> u16 {
        ordinal::NT_YIELD_EXECUTION
    }

    fn name(&self) -> &'static str {
        "NtYieldExecution"
    }

    fn stack_bytes(&self) -> u16 {
        0
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        if context.services.yield_thread() {
            KernelStatus::SUCCESS
        } else {
            // `STATUS_NO_YIELD_PERFORMED`: nothing else was ready to run.
            KernelStatus(0x4000_0024)
        }
    }
}

/// Suspends a thread, reporting its previous suspend count.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtSuspendThread;

impl KernelExport for NtSuspendThread {
    fn ordinal(&self) -> u16 {
        ordinal::NT_SUSPEND_THREAD
    }

    fn name(&self) -> &'static str {
        "NtSuspendThread"
    }

    fn stack_bytes(&self) -> u16 {
        8
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtSuspendThread(ThreadHandle, PreviousSuspendCount).
        let Some(handle) = stack_argument(context, 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let previous_out = stack_argument(context, 1).unwrap_or(0);
        match context.services.suspend_thread(handle) {
            Ok(previous) => {
                if previous_out != 0 {
                    let _ = context.memory.write_u32(GuestVa(previous_out), previous);
                }
                KernelStatus::SUCCESS
            }
            Err(_) => KernelStatus::INVALID_HANDLE,
        }
    }
}

/// Closes one guest handle (minimal object-manager surface).
#[derive(Debug, Default, Clone, Copy)]
pub struct NtClose;

impl KernelExport for NtClose {
    fn ordinal(&self) -> u16 {
        ordinal::NT_CLOSE
    }

    fn name(&self) -> &'static str {
        "NtClose"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let Some(handle) = stack_argument(context, 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        if context.services.close_handle(handle) {
            KernelStatus::SUCCESS
        } else {
            KernelStatus::INVALID_HANDLE
        }
    }
}

/// Creates a guest event object (ADR 0011: events never block; the state
/// backs the guest's signal/query bookkeeping).
#[derive(Debug, Default, Clone, Copy)]
pub struct NtCreateEvent;

impl KernelExport for NtCreateEvent {
    fn ordinal(&self) -> u16 {
        ordinal::NT_CREATE_EVENT
    }

    fn name(&self) -> &'static str {
        "NtCreateEvent"
    }

    fn stack_bytes(&self) -> u16 {
        16
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtCreateEvent(EventHandle, ObjectAttributes, EventType,
        //               InitialState); type 0 is a manual-reset notification
        //               event, 1 an auto-reset synchronization event.
        let (Some(handle_out), Some(event_type), Some(initial)) =
            (stack_argument(context, 0), stack_argument(context, 2), stack_argument(context, 3))
        else {
            return KernelStatus::INVALID_PARAMETER;
        };
        if handle_out == 0 {
            return KernelStatus::INVALID_PARAMETER;
        }
        match context.services.create_event(event_type == 0, initial & 0xFF != 0) {
            Ok(handle) => {
                let _ = context.memory.write_u32(GuestVa(handle_out), handle);
                KernelStatus::SUCCESS
            }
            Err(_) => KernelStatus::INSUFFICIENT_RESOURCES,
        }
    }
}

/// Signals a guest event object.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtSetEvent;

impl KernelExport for NtSetEvent {
    fn ordinal(&self) -> u16 {
        ordinal::NT_SET_EVENT
    }

    fn name(&self) -> &'static str {
        "NtSetEvent"
    }

    fn stack_bytes(&self) -> u16 {
        8
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtSetEvent(EventHandle, PreviousState optional).
        let Some(handle) = stack_argument(context, 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let previous_out = stack_argument(context, 1).unwrap_or(0);
        match context.services.set_event(handle) {
            Ok(previous) => {
                if previous_out != 0 {
                    let _ = context.memory.write_u32(GuestVa(previous_out), u32::from(previous));
                }
                KernelStatus::SUCCESS
            }
            Err(_) => KernelStatus::INVALID_HANDLE,
        }
    }
}

/// `STATUS_TIMEOUT`.
const STATUS_TIMEOUT: u32 = 0x0000_0102;

/// Waits on one handle (event, mutant, or thread), shared by both forms.
///
/// `timeout_index` names where the form keeps its `Timeout` argument,
/// which is honored: null waits forever, zero polls (ADR 0021).
fn wait_for_single(context: &mut KernelCallContext<'_>, timeout_index: u32) -> KernelStatus {
    let Some(handle) = stack_argument(context, 0) else {
        return KernelStatus::INVALID_PARAMETER;
    };
    let Some(timeout_ms) = read_timeout_ms(context, timeout_index) else {
        return KernelStatus::INVALID_PARAMETER;
    };
    match context.services.wait_for_object(handle, timeout_ms) {
        // A pending wait parks this thread after the export returns; the
        // saved EAX reads success now and the wake overwrites it with the
        // real outcome — the winning index or `STATUS_TIMEOUT`.
        Ok(crate::WaitOutcome::Signaled | crate::WaitOutcome::Pending) => KernelStatus::SUCCESS,
        Ok(crate::WaitOutcome::TimedOut) => KernelStatus(STATUS_TIMEOUT),
        Err(_) => KernelStatus::INVALID_HANDLE,
    }
}

/// Waits for one dispatcher object to signal.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtWaitForSingleObject;

impl KernelExport for NtWaitForSingleObject {
    fn ordinal(&self) -> u16 {
        ordinal::NT_WAIT_FOR_SINGLE_OBJECT
    }

    fn name(&self) -> &'static str {
        "NtWaitForSingleObject"
    }

    fn stack_bytes(&self) -> u16 {
        12
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtWaitForSingleObject(Handle, Alertable, Timeout).
        wait_for_single(context, 2)
    }
}

/// Waits for one dispatcher object to signal (the alertable form).
#[derive(Debug, Default, Clone, Copy)]
pub struct NtWaitForSingleObjectEx;

impl KernelExport for NtWaitForSingleObjectEx {
    fn ordinal(&self) -> u16 {
        ordinal::NT_WAIT_FOR_SINGLE_OBJECT_EX
    }

    fn name(&self) -> &'static str {
        "NtWaitForSingleObjectEx"
    }

    fn stack_bytes(&self) -> u16 {
        16
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtWaitForSingleObjectEx(Handle, WaitMode, Alertable, Timeout).
        wait_for_single(context, 3)
    }
}

/// Waits for one of several objects named by handle.
///
/// The whole set is one wait block (ADR 0021): a wait-any is satisfied by
/// the first signaled handle — checked now, or reported by the wake with
/// `STATUS_WAIT_0` plus the winner's index — and a wait-all completes only
/// when every object reads signaled at once.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtWaitForMultipleObjectsEx;

impl KernelExport for NtWaitForMultipleObjectsEx {
    fn ordinal(&self) -> u16 {
        ordinal::NT_WAIT_FOR_MULTIPLE_OBJECTS_EX
    }

    fn name(&self) -> &'static str {
        "NtWaitForMultipleObjectsEx"
    }

    fn stack_bytes(&self) -> u16 {
        24
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtWaitForMultipleObjectsEx(Count, Handles, WaitType, WaitMode,
        //                            Alertable, Timeout); WaitType 0 waits
        //                            for all, 1 for any.
        const MAXIMUM_WAIT_OBJECTS: u32 = 64;
        let (Some(count), Some(handles), Some(wait_type)) =
            (stack_argument(context, 0), stack_argument(context, 1), stack_argument(context, 2))
        else {
            return KernelStatus::INVALID_PARAMETER;
        };
        if count == 0 || count > MAXIMUM_WAIT_OBJECTS || handles == 0 {
            return KernelStatus::INVALID_PARAMETER;
        }

        let wait_all = wait_type == 0;
        let mut keys = Vec::with_capacity(count as usize);
        for index in 0..count {
            let Ok(handle) = context.memory.read_u32(GuestVa(handles + index * 4)) else {
                return KernelStatus::ACCESS_VIOLATION;
            };
            keys.push(handle);
        }
        let Some(timeout_ms) = read_timeout_ms(context, 5) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        match context.services.wait_for_objects(&keys, wait_all, timeout_ms) {
            // STATUS_WAIT_0 + index names the satisfied object.
            Ok(crate::MultiWaitOutcome::Satisfied(index)) => KernelStatus(index),
            Ok(crate::MultiWaitOutcome::TimedOut) => KernelStatus(STATUS_TIMEOUT),
            // The wake writes the real winner into the saved EAX.
            Ok(crate::MultiWaitOutcome::Pending) => KernelStatus::SUCCESS,
            Err(_) => KernelStatus::INVALID_HANDLE,
        }
    }
}

/// Reads a wait's `Timeout` argument: a guest pointer to a
/// `LARGE_INTEGER`, at `index` on the stack.
///
/// `None` means wait forever (a null pointer). `Some(0)` is a poll. A
/// negative value is a relative interval in hundred-nanosecond units,
/// converted to virtual milliseconds. A positive value is an absolute
/// time this runtime has no calendar clock to compare against; it is
/// treated as infinite with a trace note rather than converted by a
/// fabricated clock (ADR 0021).
pub(crate) fn read_timeout_ms(context: &KernelCallContext<'_>, index: u32) -> Option<Option<u64>> {
    let pointer = stack_argument(context, index)?;
    if pointer == 0 {
        return Some(None);
    }
    let low = context.memory.read_u32(GuestVa(pointer)).ok()?;
    let high = context.memory.read_u32(GuestVa(pointer.wrapping_add(4))).ok()?;
    let value = i64::from(high) << 32 | i64::from(low);
    if value == 0 {
        return Some(Some(0));
    }
    if value > 0 {
        tracing::trace!(value, "an absolute wait timeout is treated as infinite");
        return Some(None);
    }
    Some(Some((value.unsigned_abs() / 10_000).max(1)))
}

/// Reads one 32-bit stack argument above the return-address slot.
pub(crate) fn stack_argument(context: &KernelCallContext<'_>, index: u32) -> Option<u32> {
    let esp = context.cpu.gpr[4];
    let address = esp.checked_add(4)?.checked_add(index.checked_mul(4)?)?;
    context.memory.read_u32(GuestVa(address)).ok()
}

/// Reads one bounded zero-terminated guest string.
fn read_guest_string(context: &KernelCallContext<'_>, start: GuestVa) -> Option<String> {
    const LIMIT: usize = 512;

    let mut bytes = Vec::new();
    let mut address = start;
    for _ in 0..LIMIT {
        let mut byte = [0_u8];
        context.memory.read(address, &mut byte).ok()?;
        if byte[0] == 0 {
            return Some(String::from_utf8_lossy(&bytes).into_owned());
        }
        bytes.push(byte[0]);
        address = address.checked_add(1)?;
    }

    None
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::CpuState;
    use exbawks_memory::{GuestMemory, SoftwareAddressSpace};
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, MemoryPermissions};

    use super::*;

    fn memory_with_stack() -> SoftwareAddressSpace {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        memory
    }

    #[test]
    fn startup_set_registers_every_group() {
        let registry = KernelRegistry::new();
        register_startup_exports(&registry).expect("registration succeeds");

        // Nine thread/handle/event/wait exports, six Rtl and four Ke exports,
        // five Ex executive exports, nine IRQL/interrupt/PCI exports,
        // four Av video exports, one Nt virtual-memory export, seven Nt
        // file exports, six Mm exports, four symbolic-link exports, two Xe
        // section exports, one benign success export, and two startup stubs.
        assert_eq!(registry.len(), 77);
        for ordinal in [
            ordinal::DBG_PRINT,
            ordinal::HAL_RETURN_TO_FIRMWARE,
            ordinal::NT_ALLOCATE_VIRTUAL_MEMORY,
            ordinal::PS_CREATE_SYSTEM_THREAD_EX,
            ordinal::NT_CREATE_EVENT,
            ordinal::KE_SET_TIMER,
            ordinal::NT_CREATE_FILE,
            ordinal::EX_QUERY_NON_VOLATILE_SETTING,
        ] {
            assert!(registry.get(ordinal).is_some(), "ordinal {ordinal} must register");
        }
    }

    #[test]
    fn return_to_firmware_maps_the_routine_to_a_stop() {
        for (routine, expected) in [
            (0u32, StopReason::GuestExit { code: 0 }),
            (1, StopReason::Reboot { routine: 1 }),
            (2, StopReason::Reboot { routine: 2 }),
            (4, StopReason::GuestExit { code: 4 }),
        ] {
            let memory = memory_with_stack();
            memory.write_u32(GuestVa(0x2004), routine).expect("write");
            let mut cpu = CpuState::default();
            cpu.gpr[4] = 0x2000;
            let mut services = crate::UnsupportedServices;
            let mut context = KernelCallContext {
                cpu: &mut cpu,
                memory: &memory,
                services: &mut services,
                stop_request: None,
            };
            assert_eq!(HalReturnToFirmware.call(&mut context), KernelStatus::SUCCESS);
            assert_eq!(context.stop_request, Some(expected), "routine {routine}");
        }
    }

    #[test]
    fn dbg_print_reads_one_guest_string() {
        let memory = memory_with_stack();
        memory.write(GuestVa(0x1100), b"hello exbawks\0").expect("write succeeds");
        // The stack holds a return address and then the format pointer.
        memory.write_u32(GuestVa(0x2004), 0x1100).expect("write succeeds");

        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut services = crate::UnsupportedServices;
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };
        assert_eq!(DbgPrint.call(&mut context), KernelStatus::SUCCESS);
    }

    #[test]
    fn dbg_print_rejects_unreadable_pointers() {
        let memory = memory_with_stack();
        memory.write_u32(GuestVa(0x2004), 0xDEAD_0000).expect("write succeeds");

        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut services = crate::UnsupportedServices;
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };
        assert_eq!(DbgPrint.call(&mut context), KernelStatus::INVALID_PARAMETER);
    }

    #[derive(Default)]
    struct RecordingServices {
        last_request: Option<crate::ThreadCreateRequest>,
        exited: Option<u32>,
    }

    impl crate::KernelServices for RecordingServices {
        fn create_thread(
            &mut self,
            request: crate::ThreadCreateRequest,
        ) -> Result<crate::ThreadCreated, crate::KernelServiceError> {
            self.last_request = Some(request);
            Ok(crate::ThreadCreated { handle: 0xE004, thread_id: 2, kthread: GuestVa(0x8001_0200) })
        }

        fn exit_current_thread(&mut self, status: u32) {
            self.exited = Some(status);
        }

        fn close_handle(&mut self, handle: u32) -> bool {
            handle == 0xE004
        }

        fn allocate_virtual_memory(
            &mut self,
            _request: crate::VirtualAllocRequest,
        ) -> Result<crate::VirtualAllocation, crate::KernelServiceError> {
            Err(crate::KernelServiceError::Unsupported)
        }
    }

    #[test]
    fn create_thread_parses_arguments_and_writes_outputs() {
        let memory = memory_with_stack();
        // Ten stdcall arguments above the return-address slot at ESP+4.
        let arguments = [0x1200_u32, 0, 0x4000, 0, 0x1204, 0xAA, 0xBB, 0, 0, 0x1A00];
        for (index, value) in arguments.iter().enumerate() {
            memory.write_u32(GuestVa(0x2004 + index as u32 * 4), *value).expect("write");
        }
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut services = RecordingServices::default();
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };
        assert_eq!(PsCreateSystemThreadEx.call(&mut context), KernelStatus::SUCCESS);

        let request = services.last_request.expect("a request was made");
        assert_eq!(request.start_routine, GuestVa(0x1A00));
        assert_eq!(request.kernel_stack_size, 0x4000);
        assert_eq!(request.start_context1, 0xAA);
        assert_eq!(memory.read_u32(GuestVa(0x1200)).expect("handle out"), 0xE004);
        assert_eq!(memory.read_u32(GuestVa(0x1204)).expect("id out"), 2);
    }

    #[test]
    fn terminate_thread_records_the_exit_status() {
        let memory = memory_with_stack();
        memory.write_u32(GuestVa(0x2004), 0x42).expect("write");
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut services = RecordingServices::default();
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };
        assert_eq!(PsTerminateSystemThread.call(&mut context), KernelStatus::SUCCESS);
        assert_eq!(services.exited, Some(0x42));
    }

    #[test]
    fn close_reports_known_and_unknown_handles() {
        let memory = memory_with_stack();
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut services = RecordingServices::default();

        memory.write_u32(GuestVa(0x2004), 0xE004).expect("write");
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };
        assert_eq!(NtClose.call(&mut context), KernelStatus::SUCCESS);

        memory.write_u32(GuestVa(0x2004), 0x1234).expect("write");
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };
        assert_eq!(NtClose.call(&mut context), KernelStatus::INVALID_HANDLE);
    }
}
