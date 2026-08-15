use exbawks_types::{GuestVa, StopReason};

use crate::{
    KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus, StubExport,
};

/// Xbox kernel export ordinals from the public XboxDev export table.
pub mod ordinal {
    /// `DbgPrint`.
    pub const DBG_PRINT: u16 = 8;
    /// `HalReturnToFirmware`.
    pub const HAL_RETURN_TO_FIRMWARE: u16 = 49;
    /// `KeDelayExecutionThread`.
    pub const KE_DELAY_EXECUTION_THREAD: u16 = 99;
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
}

/// The startup stubs for virtual memory, threads, events, timers, and files.
const STARTUP_STUBS: [(u16, &str); 10] = [
    (ordinal::KE_DELAY_EXECUTION_THREAD, "KeDelayExecutionThread"),
    (ordinal::KE_SET_TIMER, "KeSetTimer"),
    (ordinal::NT_ALLOCATE_VIRTUAL_MEMORY, "NtAllocateVirtualMemory"),
    (ordinal::NT_CLOSE, "NtClose"),
    (ordinal::NT_CREATE_EVENT, "NtCreateEvent"),
    (ordinal::NT_CREATE_FILE, "NtCreateFile"),
    (ordinal::NT_FREE_VIRTUAL_MEMORY, "NtFreeVirtualMemory"),
    (ordinal::NT_SET_EVENT, "NtSetEvent"),
    (ordinal::PS_CREATE_SYSTEM_THREAD_EX, "PsCreateSystemThreadEx"),
    (ordinal::PS_TERMINATE_SYSTEM_THREAD, "PsTerminateSystemThread"),
];

/// Registers the startup export set for one synthetic guest thread.
pub fn register_startup_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(DbgPrint)?;
    registry.register(HalReturnToFirmware)?;
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

/// Reads one 32-bit stack argument above the return-address slot.
fn stack_argument(context: &KernelCallContext<'_>, index: u32) -> Option<u32> {
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

        assert_eq!(registry.len(), 12);
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
        let mut context = KernelCallContext { cpu: &mut cpu, memory: &memory, stop_request: None };
        assert_eq!(DbgPrint.call(&mut context), KernelStatus::SUCCESS);
    }

    #[test]
    fn dbg_print_rejects_unreadable_pointers() {
        let memory = memory_with_stack();
        memory.write_u32(GuestVa(0x2004), 0xDEAD_0000).expect("write succeeds");

        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut context = KernelCallContext { cpu: &mut cpu, memory: &memory, stop_request: None };
        assert_eq!(DbgPrint.call(&mut context), KernelStatus::INVALID_PARAMETER);
    }

    #[test]
    fn hal_return_to_firmware_requests_a_guest_exit() {
        let memory = memory_with_stack();
        memory.write_u32(GuestVa(0x2004), 2).expect("write succeeds");

        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut context = KernelCallContext { cpu: &mut cpu, memory: &memory, stop_request: None };
        assert_eq!(HalReturnToFirmware.call(&mut context), KernelStatus::SUCCESS);
        assert_eq!(context.stop_request, Some(StopReason::GuestExit { code: 2 }));
    }
}
