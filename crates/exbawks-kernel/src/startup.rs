use exbawks_types::{GuestVa, StopReason};

use crate::{
    KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus, StubExport,
    SuccessExport, ThreadCreateRequest,
};

/// Xbox kernel export ordinals from the public XboxDev export table.
pub mod ordinal {
    /// `DbgPrint`.
    pub const DBG_PRINT: u16 = 8;
    /// `HalRegisterShutdownNotification`.
    pub const HAL_REGISTER_SHUTDOWN_NOTIFICATION: u16 = 47;
    /// `HalReturnToFirmware`.
    pub const HAL_RETURN_TO_FIRMWARE: u16 = 49;
    /// `KeDelayExecutionThread`.
    pub const KE_DELAY_EXECUTION_THREAD: u16 = 99;
    /// `KeInitializeDpc`.
    pub const KE_INITIALIZE_DPC: u16 = 107;
    /// `KeInitializeTimerEx`.
    pub const KE_INITIALIZE_TIMER_EX: u16 = 113;
    /// `KeSetTimer`.
    pub const KE_SET_TIMER: u16 = 149;
    /// `NtAllocateVirtualMemory`.
    pub const NT_ALLOCATE_VIRTUAL_MEMORY: u16 = 184;
    /// `NtClose`.
    pub const NT_CLOSE: u16 = 187;
    /// `NtCreateEvent`.
    pub const NT_CREATE_EVENT: u16 = 189;
    /// `NtCreateFile`.
    pub const NT_CREATE_FILE: u16 = 190;
    /// `NtFreeVirtualMemory`.
    pub const NT_FREE_VIRTUAL_MEMORY: u16 = 199;
    /// `NtSetEvent`.
    pub const NT_SET_EVENT: u16 = 225;
    /// `PsCreateSystemThreadEx`.
    pub const PS_CREATE_SYSTEM_THREAD_EX: u16 = 255;
    /// `PsTerminateSystemThread`.
    pub const PS_TERMINATE_SYSTEM_THREAD: u16 = 258;
    /// `RtlEnterCriticalSection`.
    pub const RTL_ENTER_CRITICAL_SECTION: u16 = 277;
    /// `RtlInitializeCriticalSection`.
    pub const RTL_INITIALIZE_CRITICAL_SECTION: u16 = 291;
    /// `RtlLeaveCriticalSection`.
    pub const RTL_LEAVE_CRITICAL_SECTION: u16 = 294;
}

/// The startup stubs for virtual memory, events, timers, and files.
const STARTUP_STUBS: [(u16, &str); 7] = [
    (ordinal::KE_DELAY_EXECUTION_THREAD, "KeDelayExecutionThread"),
    (ordinal::KE_SET_TIMER, "KeSetTimer"),
    (ordinal::NT_ALLOCATE_VIRTUAL_MEMORY, "NtAllocateVirtualMemory"),
    (ordinal::NT_CREATE_EVENT, "NtCreateEvent"),
    (ordinal::NT_CREATE_FILE, "NtCreateFile"),
    (ordinal::NT_FREE_VIRTUAL_MEMORY, "NtFreeVirtualMemory"),
    (ordinal::NT_SET_EVENT, "NtSetEvent"),
];

/// Benign exports that succeed as no-ops on the boot path:
/// (ordinal, name, stdcall argument bytes).
const BENIGN_SUCCESS: [(u16, &str, u16); 1] =
    [(ordinal::HAL_REGISTER_SHUTDOWN_NOTIFICATION, "HalRegisterShutdownNotification", 8)];

/// Registers the startup export set for one synthetic guest thread.
pub fn register_startup_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(DbgPrint)?;
    registry.register(HalReturnToFirmware)?;
    registry.register(PsCreateSystemThreadEx)?;
    registry.register(PsTerminateSystemThread)?;
    registry.register(NtClose)?;
    crate::rtl::register_rtl_exports(registry)?;
    crate::ke::register_ke_exports(registry)?;
    for (ordinal, name, stack_bytes) in BENIGN_SUCCESS {
        registry.register(SuccessExport::new(ordinal, name, stack_bytes))?;
    }
    for (ordinal, name) in STARTUP_STUBS {
        registry.register(StubExport::new(ordinal, name))?;
    }
    Ok(())
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
        context.stop_request = Some(StopReason::GuestExit { code: routine });
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

        // Five thread/handle exports, three Rtl and two Ke dispatcher
        // exports, one benign success export, and seven startup stubs.
        assert_eq!(registry.len(), 18);
        for ordinal in [
            ordinal::DBG_PRINT,
            ordinal::HAL_RETURN_TO_FIRMWARE,
            ordinal::NT_ALLOCATE_VIRTUAL_MEMORY,
            ordinal::PS_CREATE_SYSTEM_THREAD_EX,
            ordinal::NT_CREATE_EVENT,
            ordinal::KE_SET_TIMER,
            ordinal::NT_CREATE_FILE,
        ] {
            assert!(registry.get(ordinal).is_some(), "ordinal {ordinal} must register");
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

    #[test]
    fn hal_return_to_firmware_requests_a_guest_exit() {
        let memory = memory_with_stack();
        memory.write_u32(GuestVa(0x2004), 2).expect("write succeeds");

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
        assert_eq!(context.stop_request, Some(StopReason::GuestExit { code: 2 }));
    }
}
