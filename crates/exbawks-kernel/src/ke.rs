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
    registry.register(KeInsertQueueDpc)?;
    registry.register(KeInitializeTimerEx)?;
    registry.register(KeSetTimer)?;
    registry.register(KeQuerySystemTime)?;
    Ok(())
}

/// The virtual clock's epoch: 2004-01-01 00:00:00 as an NT `FILETIME`
/// (100 ns units since 1601-01-01).
const SYSTEM_TIME_EPOCH: u64 = 127_173_888_000_000_000;

/// Returns the deterministic virtual system time.
///
/// Derived from the virtualized time-stamp counter (one tick per retired
/// instruction, read as 100 ns units), so runs are reproducible and no host
/// clock leaks into guest-visible state. The virtual-clock design (boot plan
/// D3) will formalize the scaling.
fn virtual_system_time(context: &KernelCallContext<'_>) -> u64 {
    SYSTEM_TIME_EPOCH.wrapping_add(context.cpu.tsc)
}

/// Writes the current system time as an NT `FILETIME`.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeQuerySystemTime;

impl KernelExport for KeQuerySystemTime {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_QUERY_SYSTEM_TIME
    }

    fn name(&self) -> &'static str {
        "KeQuerySystemTime"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let Some(out) = stack_argument(context, 0).filter(|pointer| *pointer != 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let time = virtual_system_time(context);
        let _ = context.memory.write_u32(GuestVa(out), time as u32);
        let _ = context.memory.write_u32(GuestVa(out + 4), (time >> 32) as u32);
        KernelStatus::SUCCESS
    }
}

/// `KTIMER.DueTime` (a `LARGE_INTEGER`) and `KTIMER.Dpc` field offsets.
const TIMER_DUE_TIME: u32 = 0x10;
const TIMER_DPC: u32 = 0x20;

/// Queues a guest `KDPC` for the runtime to call.
///
/// An interrupt service routine does almost nothing itself: it
/// acknowledges the device and queues this, and the deferred routine does
/// the work at a lower interrupt level. A driver whose deferred routine
/// never runs has serviced the interrupt and thrown the result away.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeInsertQueueDpc;

impl KernelExport for KeInsertQueueDpc {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_INSERT_QUEUE_DPC
    }

    fn name(&self) -> &'static str {
        "KeInsertQueueDpc"
    }

    fn stack_bytes(&self) -> u16 {
        12
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let (Some(dpc), Some(first), Some(second)) =
            (stack_argument(context, 0), stack_argument(context, 1), stack_argument(context, 2))
        else {
            return KernelStatus(0);
        };
        if dpc == 0 {
            return KernelStatus(0);
        }
        // The caller's two arguments are held in the object until the
        // deferred routine is called with them.
        let _ = context.memory.write_u32(GuestVa(dpc + 0x14), first);
        let _ = context.memory.write_u32(GuestVa(dpc + 0x18), second);
        // BOOLEAN: TRUE when the object was not already queued.
        KernelStatus(u32::from(context.services.queue_dpc(dpc)))
    }
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

/// Arms a guest `KTIMER` with a due time and optional DPC.
///
/// `KeSetTimer(Timer, DueTime, Dpc)` normally inserts the timer into the
/// system timer queue and returns whether it was already queued. Under the
/// cooperative single-thread scheduler (ADR 0011) there is no timer queue
/// yet — the full firing machinery is `KRN-007` — so this records the due
/// time and DPC on the object and reports the timer was not already set
/// (`FALSE`). A title that merely arms fire-and-forget timers proceeds; one
/// that blocks waiting for a timer to signal is later work.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeSetTimer;

impl KernelExport for KeSetTimer {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_SET_TIMER
    }

    fn name(&self) -> &'static str {
        "KeSetTimer"
    }

    fn stack_bytes(&self) -> u16 {
        16
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let Some(timer) = stack_argument(context, 0).filter(|pointer| *pointer != 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        // DueTime is a LARGE_INTEGER passed by value (two args); Dpc follows.
        let due_low = stack_argument(context, 1).unwrap_or(0);
        let due_high = stack_argument(context, 2).unwrap_or(0);
        let dpc = stack_argument(context, 3).unwrap_or(0);
        let _ = context.memory.write_u32(GuestVa(timer + TIMER_DUE_TIME), due_low);
        let _ = context.memory.write_u32(GuestVa(timer + TIMER_DUE_TIME + 4), due_high);
        let _ = context.memory.write_u32(GuestVa(timer + TIMER_DPC), dpc);
        // BOOLEAN FALSE: the timer was not already in the (empty) queue.
        KernelStatus(0)
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

    /// A services object that records what was queued.
    #[derive(Default)]
    struct RecordingServices {
        queued: Vec<u32>,
    }

    impl crate::KernelServices for RecordingServices {
        fn create_thread(
            &mut self,
            _request: crate::ThreadCreateRequest,
        ) -> Result<crate::ThreadCreated, crate::KernelServiceError> {
            Err(crate::KernelServiceError::Unsupported)
        }

        fn exit_current_thread(&mut self, _status: u32) {}

        fn close_handle(&mut self, _handle: u32) -> bool {
            false
        }

        fn allocate_virtual_memory(
            &mut self,
            _request: crate::VirtualAllocRequest,
        ) -> Result<crate::VirtualAllocation, crate::KernelServiceError> {
            Err(crate::KernelServiceError::Unsupported)
        }

        fn queue_dpc(&mut self, dpc: u32) -> bool {
            if self.queued.contains(&dpc) {
                return false;
            }
            self.queued.push(dpc);
            true
        }
    }

    #[test]
    fn insert_queue_dpc_stores_its_arguments_and_queues_once() {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        let dpc = 0x1100_u32;
        // [esp]=return, then Dpc, SystemArgument1, SystemArgument2.
        for (slot, value) in [dpc, 0x1111_2222, 0x3333_4444].iter().enumerate() {
            memory.write_u32(GuestVa(0x2004 + slot as u32 * 4), *value).expect("write");
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

        // The deferred routine is called with these later, so they have to
        // reach the object rather than being dropped.
        assert_eq!(KeInsertQueueDpc.call(&mut context), KernelStatus(1));
        assert_eq!(memory.read_u32(GuestVa(dpc + 0x14)).unwrap(), 0x1111_2222);
        assert_eq!(memory.read_u32(GuestVa(dpc + 0x18)).unwrap(), 0x3333_4444);
        assert_eq!(services.queued, vec![dpc]);

        // Queueing the same object again reports false, as the kernel does
        // for one already on the queue.
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };
        assert_eq!(KeInsertQueueDpc.call(&mut context), KernelStatus(0));
        assert_eq!(services.queued, vec![dpc], "and it is not queued twice");
    }

    #[test]
    fn insert_queue_dpc_refuses_a_null_object() {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        for slot in 0..3 {
            memory.write_u32(GuestVa(0x2004 + slot * 4), 0).expect("write");
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

        assert_eq!(KeInsertQueueDpc.call(&mut context), KernelStatus(0));
        assert!(services.queued.is_empty(), "a null object queues nothing");
    }

    #[test]
    fn set_timer_records_the_due_time_and_returns_false() {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        let timer = 0x1100_u32;
        // [esp]=return, then Timer, DueTime.Low, DueTime.High, Dpc.
        for (slot, value) in [timer, 0xDEAD_BEEF, 0x0000_0001, 0x2200].iter().enumerate() {
            memory.write_u32(GuestVa(0x2004 + slot as u32 * 4), *value).expect("write");
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

        assert_eq!(KeSetTimer.call(&mut context), KernelStatus(0), "the timer was not queued");
        assert_eq!(memory.read_u32(GuestVa(timer + TIMER_DUE_TIME)).unwrap(), 0xDEAD_BEEF);
        assert_eq!(memory.read_u32(GuestVa(timer + TIMER_DUE_TIME + 4)).unwrap(), 1);
        assert_eq!(memory.read_u32(GuestVa(timer + TIMER_DPC)).unwrap(), 0x2200);
    }
}
