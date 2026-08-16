//! `Ke*` waits on guest dispatcher objects.
//!
//! The `Nt*` wait exports name an object by handle; the `Ke*` pair here
//! names one by pointer — the guest owns the `DISPATCHER_HEADER` and reads
//! its signal state directly, so these exports work on guest memory first
//! and only fall back to the scheduler when a wait would actually block.
//!
//! Nothing in the emulator raises an interrupt, so a wait no runnable
//! thread can ever satisfy completes instead of deadlocking: the device
//! model finishes submitted work synchronously (a graphics fence is
//! already retired by the time the title waits on it), which makes
//! "signaled" the truthful answer rather than an optimistic one.

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
use crate::{
    KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus, WaitOutcome,
};

/// `DISPATCHER_HEADER.SignalState`.
const SIGNAL_STATE_OFFSET: u32 = 4;
/// `DISPATCHER_HEADER.Type` for an auto-reset (synchronization) event.
const SYNCHRONIZATION_EVENT: u32 = 1;

/// Registers the dispatcher-object exports.
pub(crate) fn register_dispatcher_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(KeSetEvent)?;
    registry.register(KeWaitForSingleObject)?;
    Ok(())
}

/// Reads a dispatcher object's signal state.
fn signal_state(context: &KernelCallContext<'_>, object: u32) -> Option<u32> {
    context.memory.read_u32(GuestVa(object + SIGNAL_STATE_OFFSET)).ok()
}

/// Waits for one dispatcher object named by pointer.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeWaitForSingleObject;

impl KernelExport for KeWaitForSingleObject {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_WAIT_FOR_SINGLE_OBJECT
    }

    fn name(&self) -> &'static str {
        "KeWaitForSingleObject"
    }

    fn stack_bytes(&self) -> u16 {
        20
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // KeWaitForSingleObject(Object, WaitReason, WaitMode, Alertable,
        //                       Timeout).
        let Some(object) = stack_argument(context, 0).filter(|object| *object != 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let Some(state) = signal_state(context, object) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        if state != 0 {
            // An auto-reset event's signal is consumed by this waiter.
            let kind = context.memory.read_u32(GuestVa(object)).unwrap_or(0) & 0xFF;
            if kind == SYNCHRONIZATION_EVENT {
                let _ = context.memory.write_u32(GuestVa(object + SIGNAL_STATE_OFFSET), 0);
            }
            return KernelStatus::SUCCESS;
        }
        match context.services.wait_for_dispatcher_object(object) {
            // A pending wait parks this thread once the export returns; the
            // saved status must already read success for the wake.
            Ok(WaitOutcome::Signaled | WaitOutcome::Pending) => KernelStatus::SUCCESS,
            Ok(WaitOutcome::TimedOut) => {
                tracing::debug!(
                    object = format_args!("{object:#010x}"),
                    "KeWaitForSingleObject: no thread can signal; completing the wait"
                );
                KernelStatus::SUCCESS
            }
            Err(_) => KernelStatus::INVALID_PARAMETER,
        }
    }
}

/// Signals a dispatcher event named by pointer, returning its old state.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeSetEvent;

impl KernelExport for KeSetEvent {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_SET_EVENT
    }

    fn name(&self) -> &'static str {
        "KeSetEvent"
    }

    fn stack_bytes(&self) -> u16 {
        12
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // KeSetEvent(Event, Increment, Wait).
        let Some(event) = stack_argument(context, 0).filter(|event| *event != 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let Some(previous) = signal_state(context, event) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let _ = context.memory.write_u32(GuestVa(event + SIGNAL_STATE_OFFSET), 1);
        context.services.signal_dispatcher_object(event);
        // The previous state returns in EAX.
        KernelStatus(previous)
    }
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::CpuState;
    use exbawks_memory::{GuestMemory, SoftwareAddressSpace};
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, MemoryPermissions};

    use crate::{KernelServiceError, KernelServices};

    use super::*;

    /// A services stub recording the wait and signal it receives.
    #[derive(Debug, Default)]
    struct WaitServices {
        outcome: Option<WaitOutcome>,
        waited: Option<u32>,
        signaled: Option<u32>,
    }

    impl KernelServices for WaitServices {
        fn create_thread(
            &mut self,
            _request: crate::ThreadCreateRequest,
        ) -> Result<crate::ThreadCreated, KernelServiceError> {
            Err(KernelServiceError::Unsupported)
        }

        fn allocate_virtual_memory(
            &mut self,
            _request: crate::VirtualAllocRequest,
        ) -> Result<crate::VirtualAllocation, KernelServiceError> {
            Err(KernelServiceError::Unsupported)
        }

        fn exit_current_thread(&mut self, _status: u32) {}

        fn close_handle(&mut self, _handle: u32) -> bool {
            false
        }

        fn wait_for_dispatcher_object(
            &mut self,
            address: u32,
        ) -> Result<WaitOutcome, KernelServiceError> {
            self.waited = Some(address);
            self.outcome.ok_or(KernelServiceError::Unsupported)
        }

        fn signal_dispatcher_object(&mut self, address: u32) {
            self.signaled = Some(address);
        }
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

    /// Lays out `argument` values above the return slot and runs `export`.
    fn call(
        export: &dyn KernelExport,
        arguments: &[u32],
        services: &mut dyn KernelServices,
        memory: &SoftwareAddressSpace,
    ) -> KernelStatus {
        for (slot, value) in arguments.iter().enumerate() {
            memory.write_u32(GuestVa(0x2004 + slot as u32 * 4), *value).expect("write");
        }
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut context = KernelCallContext { cpu: &mut cpu, memory, services, stop_request: None };
        export.call(&mut context)
    }

    #[test]
    fn a_signaled_synchronization_event_is_consumed() {
        let memory = mapped_memory();
        // A synchronization event (type 1), signaled.
        memory.write_u32(GuestVa(0x3000), SYNCHRONIZATION_EVENT).expect("type");
        memory.write_u32(GuestVa(0x3004), 1).expect("state");
        let mut services = WaitServices::default();
        assert_eq!(
            call(&KeWaitForSingleObject, &[0x3000, 0, 0, 0, 0], &mut services, &memory),
            KernelStatus::SUCCESS
        );
        assert_eq!(memory.read_u32(GuestVa(0x3004)).expect("state"), 0, "the signal is consumed");
        assert_eq!(services.waited, None, "a signaled object never reaches the scheduler");
    }

    #[test]
    fn a_signaled_notification_event_stays_signaled() {
        let memory = mapped_memory();
        // A notification event (type 0) keeps its state for other waiters.
        memory.write_u32(GuestVa(0x3000), 0).expect("type");
        memory.write_u32(GuestVa(0x3004), 1).expect("state");
        let mut services = WaitServices::default();
        assert_eq!(
            call(&KeWaitForSingleObject, &[0x3000, 0, 0, 0, 0], &mut services, &memory),
            KernelStatus::SUCCESS
        );
        assert_eq!(memory.read_u32(GuestVa(0x3004)).expect("state"), 1);
    }

    #[test]
    fn an_unsignaled_object_reaches_the_scheduler() {
        let memory = mapped_memory();
        memory.write_u32(GuestVa(0x3000), SYNCHRONIZATION_EVENT).expect("type");
        memory.write_u32(GuestVa(0x3004), 0).expect("state");
        let mut services =
            WaitServices { outcome: Some(WaitOutcome::Pending), ..Default::default() };
        assert_eq!(
            call(&KeWaitForSingleObject, &[0x3000, 0, 0, 0, 0], &mut services, &memory),
            KernelStatus::SUCCESS
        );
        assert_eq!(services.waited, Some(0x3000));
    }

    #[test]
    fn an_unsatisfiable_wait_completes_instead_of_deadlocking() {
        let memory = mapped_memory();
        memory.write_u32(GuestVa(0x3000), SYNCHRONIZATION_EVENT).expect("type");
        memory.write_u32(GuestVa(0x3004), 0).expect("state");
        let mut services =
            WaitServices { outcome: Some(WaitOutcome::TimedOut), ..Default::default() };
        assert_eq!(
            call(&KeWaitForSingleObject, &[0x3000, 0, 0, 0, 0], &mut services, &memory),
            KernelStatus::SUCCESS
        );
    }

    #[test]
    fn setting_an_event_signals_it_and_reports_the_previous_state() {
        let memory = mapped_memory();
        memory.write_u32(GuestVa(0x3000), SYNCHRONIZATION_EVENT).expect("type");
        memory.write_u32(GuestVa(0x3004), 0).expect("state");
        let mut services = WaitServices::default();
        assert_eq!(
            call(&KeSetEvent, &[0x3000, 0, 0], &mut services, &memory),
            KernelStatus(0),
            "the event was clear"
        );
        assert_eq!(memory.read_u32(GuestVa(0x3004)).expect("state"), 1);
        assert_eq!(services.signaled, Some(0x3000), "parked waiters are woken");
    }

    #[test]
    fn a_null_object_is_rejected() {
        let memory = mapped_memory();
        let mut services = WaitServices::default();
        assert_eq!(
            call(&KeWaitForSingleObject, &[0, 0, 0, 0, 0], &mut services, &memory),
            KernelStatus::INVALID_PARAMETER
        );
        assert_eq!(
            call(&KeSetEvent, &[0, 0, 0], &mut services, &memory),
            KernelStatus::INVALID_PARAMETER
        );
    }
}
