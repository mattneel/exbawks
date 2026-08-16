//! Rtl runtime-library exports (HLE-007).
//!
//! The critical-section family operates on a guest-resident
//! `RTL_CRITICAL_SECTION`. Under the cooperative single-thread scheduler
//! (ADR 0011) only one guest thread runs at a time and threads switch only at
//! kernel dispatch points, so a critical section is never contended while a
//! thread holds it; these exports therefore maintain the lock and recursion
//! counts for the guest's own bookkeeping without ever blocking.

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
use crate::{KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus};

/// Xbox `RTL_CRITICAL_SECTION` field offsets: a 16-byte `DISPATCHER_HEADER`
/// event, then the counts and owner.
const LOCK_COUNT: u32 = 0x10;
const RECURSION_COUNT: u32 = 0x14;
const OWNING_THREAD: u32 = 0x18;

/// The placeholder owner id for the single running thread.
const CURRENT_THREAD: u32 = 1;

/// Registers the Rtl critical-section exports.
pub(crate) fn register_rtl_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(RtlInitializeCriticalSection)?;
    registry.register(RtlEnterCriticalSection)?;
    registry.register(RtlLeaveCriticalSection)?;
    Ok(())
}

/// Reads the critical-section pointer argument, when readable.
fn critical_section(context: &KernelCallContext<'_>) -> Option<u32> {
    stack_argument(context, 0).filter(|pointer| *pointer != 0)
}

fn field(context: &KernelCallContext<'_>, base: u32, offset: u32) -> u32 {
    context.memory.read_u32(GuestVa(base.wrapping_add(offset))).unwrap_or(0)
}

fn set_field(context: &mut KernelCallContext<'_>, base: u32, offset: u32, value: u32) {
    // A faulting write means the guest passed a bad pointer; the export is
    // void, so best-effort is correct and never panics.
    let _ = context.memory.write_u32(GuestVa(base.wrapping_add(offset)), value);
}

/// Initializes one critical section to the unlocked state.
#[derive(Debug, Default, Clone, Copy)]
pub struct RtlInitializeCriticalSection;

impl KernelExport for RtlInitializeCriticalSection {
    fn ordinal(&self) -> u16 {
        crate::ordinal::RTL_INITIALIZE_CRITICAL_SECTION
    }

    fn name(&self) -> &'static str {
        "RtlInitializeCriticalSection"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let Some(base) = critical_section(context) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        set_field(context, base, LOCK_COUNT, 0);
        set_field(context, base, RECURSION_COUNT, 0);
        set_field(context, base, OWNING_THREAD, 0);
        KernelStatus::SUCCESS
    }
}

/// Acquires one critical section, counting recursive entries.
#[derive(Debug, Default, Clone, Copy)]
pub struct RtlEnterCriticalSection;

impl KernelExport for RtlEnterCriticalSection {
    fn ordinal(&self) -> u16 {
        crate::ordinal::RTL_ENTER_CRITICAL_SECTION
    }

    fn name(&self) -> &'static str {
        "RtlEnterCriticalSection"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let Some(base) = critical_section(context) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let recursion = field(context, base, RECURSION_COUNT);
        let lock = field(context, base, LOCK_COUNT);
        set_field(context, base, RECURSION_COUNT, recursion.wrapping_add(1));
        set_field(context, base, LOCK_COUNT, lock.wrapping_add(1));
        set_field(context, base, OWNING_THREAD, CURRENT_THREAD);
        KernelStatus::SUCCESS
    }
}

/// Releases one critical section, balancing recursive entries.
#[derive(Debug, Default, Clone, Copy)]
pub struct RtlLeaveCriticalSection;

impl KernelExport for RtlLeaveCriticalSection {
    fn ordinal(&self) -> u16 {
        crate::ordinal::RTL_LEAVE_CRITICAL_SECTION
    }

    fn name(&self) -> &'static str {
        "RtlLeaveCriticalSection"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let Some(base) = critical_section(context) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let recursion = field(context, base, RECURSION_COUNT).saturating_sub(1);
        let lock = field(context, base, LOCK_COUNT).saturating_sub(1);
        set_field(context, base, RECURSION_COUNT, recursion);
        set_field(context, base, LOCK_COUNT, lock);
        if recursion == 0 {
            set_field(context, base, OWNING_THREAD, 0);
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

    fn context_at(cs: u32) -> (SoftwareAddressSpace, CpuState) {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 2 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        // Stack: return address at [esp], the CS pointer at [esp + 4].
        memory.write_u32(GuestVa(0x2004), cs).expect("write");
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        (memory, cpu)
    }

    #[test]
    fn enter_and_leave_balance_the_recursion_count() {
        let cs = 0x1100;
        let (memory, mut cpu) = context_at(cs);
        let mut services = UnsupportedServices;
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };

        assert_eq!(RtlInitializeCriticalSection.call(&mut context), KernelStatus::SUCCESS);
        assert_eq!(memory.read_u32(GuestVa(cs + RECURSION_COUNT)).unwrap(), 0);

        RtlEnterCriticalSection.call(&mut context);
        RtlEnterCriticalSection.call(&mut context);
        assert_eq!(memory.read_u32(GuestVa(cs + RECURSION_COUNT)).unwrap(), 2);
        assert_eq!(memory.read_u32(GuestVa(cs + OWNING_THREAD)).unwrap(), CURRENT_THREAD);

        RtlLeaveCriticalSection.call(&mut context);
        assert_eq!(memory.read_u32(GuestVa(cs + RECURSION_COUNT)).unwrap(), 1);
        RtlLeaveCriticalSection.call(&mut context);
        assert_eq!(memory.read_u32(GuestVa(cs + RECURSION_COUNT)).unwrap(), 0);
        assert_eq!(
            memory.read_u32(GuestVa(cs + OWNING_THREAD)).unwrap(),
            0,
            "unowned when balanced"
        );
    }

    #[test]
    fn a_null_section_is_rejected() {
        let (memory, mut cpu) = context_at(0);
        let mut services = UnsupportedServices;
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };
        assert_eq!(RtlEnterCriticalSection.call(&mut context), KernelStatus::INVALID_PARAMETER);
    }
}
