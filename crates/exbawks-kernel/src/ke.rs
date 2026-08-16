//! Ke dispatcher-object exports.
//!
//! These initialize guest-resident kernel objects (DPCs first). Under the
//! cooperative scheduler (ADR 0011) the objects do not fire yet; the full
//! DPC/timer machinery is `KRN-007`. Initializing the structs keeps the
//! guest's own bookkeeping consistent so boot proceeds.

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
use crate::{KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus};

/// `KDPC.Type` for a DPC object.
const DPC_OBJECT_TYPE: u32 = 0x13;
/// `DISPATCHER_HEADER.Type` for a notification timer.
const NOTIFICATION_TIMER_TYPE: u32 = 8;

/// Registers the Ke dispatcher-object exports.
pub(crate) fn register_ke_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(KeInitializeDpc)?;
    registry.register(KeInitializeTimerEx)?;
    Ok(())
}

/// Initializes a guest `KDPC` with its deferred routine and context.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeInitializeDpc;

impl KernelExport for KeInitializeDpc {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_INITIALIZE_DPC
    }

    fn name(&self) -> &'static str {
        "KeInitializeDpc"
    }

    fn stack_bytes(&self) -> u16 {
        12
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let (Some(dpc), Some(routine), Some(deferred_context)) =
            (stack_argument(context, 0), stack_argument(context, 1), stack_argument(context, 2))
        else {
            return KernelStatus::INVALID_PARAMETER;
        };
        if dpc == 0 {
            return KernelStatus::INVALID_PARAMETER;
        }

        // KDPC: Type@0x00, DpcListEntry@0x04 (Flink/Blink), DeferredRoutine
        // @0x0C, DeferredContext@0x10. Best-effort writes; a bad pointer is
        // the guest's error and the export is void.
        let _ = context.memory.write_u32(GuestVa(dpc), DPC_OBJECT_TYPE);
        let _ = context.memory.write_u32(GuestVa(dpc + 0x04), 0);
        let _ = context.memory.write_u32(GuestVa(dpc + 0x08), 0);
        let _ = context.memory.write_u32(GuestVa(dpc + 0x0C), routine);
        let _ = context.memory.write_u32(GuestVa(dpc + 0x10), deferred_context);
        KernelStatus::SUCCESS
    }
}

/// Initializes a guest `KTIMER` to the unset state.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeInitializeTimerEx;

impl KernelExport for KeInitializeTimerEx {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_INITIALIZE_TIMER_EX
    }

    fn name(&self) -> &'static str {
        "KeInitializeTimerEx"
    }

    fn stack_bytes(&self) -> u16 {
        8
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let (Some(timer), Some(timer_type)) =
            (stack_argument(context, 0), stack_argument(context, 1))
        else {
            return KernelStatus::INVALID_PARAMETER;
        };
        if timer == 0 {
            return KernelStatus::INVALID_PARAMETER;
        }

        // KTIMER: DISPATCHER_HEADER@0x00 (Type/Size/SignalState/WaitListHead),
        // DueTime@0x10, TimerListEntry@0x18, Dpc@0x20, Period@0x24. A
        // synchronization timer sets Type 9, a notification timer 8.
        let header = NOTIFICATION_TIMER_TYPE | (timer_type & 1) | (0x000A << 16);
        let _ = context.memory.write_u32(GuestVa(timer), header); // Type/Size
        let _ = context.memory.write_u32(GuestVa(timer + 0x04), 0); // SignalState
        // Empty wait list is self-referential.
        let _ = context.memory.write_u32(GuestVa(timer + 0x08), timer + 0x08);
        let _ = context.memory.write_u32(GuestVa(timer + 0x0C), timer + 0x08);
        for offset in [0x10, 0x14, 0x18, 0x1C, 0x20, 0x24] {
            let _ = context.memory.write_u32(GuestVa(timer + offset), 0);
        }
        KernelStatus::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::CpuState;
    use exbawks_memory::{GuestMemory, SoftwareAddressSpace};
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, MemoryPermissions};

    use crate::UnsupportedServices;

    use super::*;

    #[test]
    fn initialize_dpc_writes_the_routine_and_context() {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        let dpc = 0x1100_u32;
        for (index, value) in [dpc, 0xAABB_CCDD, 0x1234_5678].iter().enumerate() {
            memory.write_u32(GuestVa(0x2004 + index as u32 * 4), *value).expect("write");
        }
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut services = UnsupportedServices;
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };

        assert_eq!(KeInitializeDpc.call(&mut context), KernelStatus::SUCCESS);
        assert_eq!(memory.read_u32(GuestVa(dpc)).unwrap(), DPC_OBJECT_TYPE);
        assert_eq!(memory.read_u32(GuestVa(dpc + 0x0C)).unwrap(), 0xAABB_CCDD);
        assert_eq!(memory.read_u32(GuestVa(dpc + 0x10)).unwrap(), 0x1234_5678);
    }
}
