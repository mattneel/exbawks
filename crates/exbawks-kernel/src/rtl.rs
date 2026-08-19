//! Rtl runtime-library exports (HLE-007).
//!
//! The critical-section family operates on a guest-resident
//! `RTL_CRITICAL_SECTION`. Threads switch only at kernel dispatch points,
//! but entering a section IS one — and so is every allocation a title makes
//! inside one — so a section can be contended and must block. It counts
//! from minus one, as the structure does: incrementing to zero takes it,
//! anything above is a waiter, and the first sixteen bytes are the event a
//! releasing thread signals.
//!
//! `RtlNtStatusToDosError` is a pure status-code translator titles call from
//! their error-reporting paths.

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
use crate::{KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus};

/// Xbox `RTL_CRITICAL_SECTION` field offsets: a 16-byte `DISPATCHER_HEADER`
/// event, then the counts and owner.
const LOCK_COUNT: u32 = 0x10;
const RECURSION_COUNT: u32 = 0x14;
const OWNING_THREAD: u32 = 0x18;

/// A free critical section's `LockCount`.
///
/// It counts waiters from minus one rather than holders from zero, so
/// incrementing it to zero is what acquires the section. This is the
/// structure's own convention, and a title that reads the field expects it.
const UNOWNED_LOCK_COUNT: u32 = u32::MAX;

/// The pseudo-handle naming whichever thread is running, which is how a
/// section learns its owner.
const CURRENT_THREAD_PSEUDO_HANDLE: u32 = 0xFFFF_FFFE;

/// The control block of the thread asking, or `None` where the runtime
/// keeps no thread table (the export then behaves as it did before there
/// was one to ask).
fn current_thread(context: &KernelCallContext<'_>) -> Option<u32> {
    context.services.object_for_handle(CURRENT_THREAD_PSEUDO_HANDLE)
}

/// Registers the Rtl runtime-library exports.
pub(crate) fn register_rtl_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(RtlInitializeCriticalSection)?;
    registry.register(RtlEnterCriticalSection)?;
    registry.register(RtlLeaveCriticalSection)?;
    registry.register(RtlNtStatusToDosError)?;
    registry.register(RtlInitAnsiString)?;
    registry.register(RtlEqualString)?;
    Ok(())
}

/// The generic Win32 error for an NTSTATUS with no specific mapping
/// (`ERROR_MR_MID_NOT_FOUND`).
const ERROR_MR_MID_NOT_FOUND: u32 = 317;

/// Translates an NTSTATUS to its Win32/DOS error code.
///
/// This is the well-known subset titles actually hit on their error paths;
/// a success or informational status maps to `NO_ERROR`, and an unmapped
/// failure to the generic `ERROR_MR_MID_NOT_FOUND`, matching the kernel's
/// own fall-through.
fn dos_error(status: u32) -> u32 {
    match status {
        0x0000_0000 => 0,    // STATUS_SUCCESS -> NO_ERROR
        0xC000_0002 => 120,  // NOT_IMPLEMENTED -> ERROR_CALL_NOT_IMPLEMENTED
        0xC000_0005 => 998,  // ACCESS_VIOLATION -> ERROR_NOACCESS
        0xC000_0008 => 6,    // INVALID_HANDLE -> ERROR_INVALID_HANDLE
        0xC000_000D => 87,   // INVALID_PARAMETER -> ERROR_INVALID_PARAMETER
        0xC000_000F => 2,    // NO_SUCH_FILE -> ERROR_FILE_NOT_FOUND
        0xC000_0011 => 38,   // END_OF_FILE -> ERROR_HANDLE_EOF
        0xC000_0017 => 8,    // NO_MEMORY -> ERROR_NOT_ENOUGH_MEMORY
        0xC000_0023 => 122,  // BUFFER_TOO_SMALL -> ERROR_INSUFFICIENT_BUFFER
        0xC000_0034 => 2,    // OBJECT_NAME_NOT_FOUND -> ERROR_FILE_NOT_FOUND
        0xC000_0035 => 183,  // OBJECT_NAME_COLLISION -> ERROR_ALREADY_EXISTS
        0xC000_003A => 3,    // OBJECT_PATH_NOT_FOUND -> ERROR_PATH_NOT_FOUND
        0xC000_009A => 1450, // INSUFFICIENT_RESOURCES -> ERROR_NO_SYSTEM_RESOURCES
        _ if status >> 31 == 0 => 0,
        _ => ERROR_MR_MID_NOT_FOUND,
    }
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
        set_field(context, base, LOCK_COUNT, UNOWNED_LOCK_COUNT);
        set_field(context, base, RECURSION_COUNT, 0);
        set_field(context, base, OWNING_THREAD, 0);
        // The first sixteen bytes are the event a releasing thread signals,
        // and an empty wait list points at itself.
        set_field(context, base, 0x00, 0);
        set_field(context, base, 0x04, 0);
        set_field(context, base, 0x08, base.wrapping_add(0x08));
        set_field(context, base, 0x0C, base.wrapping_add(0x08));
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
        // Counting up from minus one: reaching zero is what takes the
        // section, and anything above it is a thread that has to wait.
        let lock = field(context, base, LOCK_COUNT).wrapping_add(1);
        set_field(context, base, LOCK_COUNT, lock);
        let owner = current_thread(context);
        if lock == 0 {
            set_field(context, base, OWNING_THREAD, owner.unwrap_or(0));
            set_field(context, base, RECURSION_COUNT, 1);
            return KernelStatus::SUCCESS;
        }

        let held_by = field(context, base, OWNING_THREAD);
        if owner.is_some_and(|thread| thread == held_by) {
            // The same thread again: a critical section is recursive, and
            // taking it twice must not wait on itself.
            let recursion = field(context, base, RECURSION_COUNT);
            set_field(context, base, RECURSION_COUNT, recursion.wrapping_add(1));
            return KernelStatus::SUCCESS;
        }

        // Another thread holds it. Waiting here is the whole point of the
        // export: a section that lets a second thread in guards nothing,
        // and what it usually guards is a heap — where two threads at once
        // means a free list with a link through a freed block, found much
        // later and nowhere near the cause.
        //
        // The section is waited on by its own address, as the structure
        // intends: its first sixteen bytes are the event the releasing
        // thread signals.
        match context.services.wait_for_dispatcher_object(base, None) {
            // Parked. The caller resumes only after the releasing thread
            // hands it the section: the release stamps this thread's
            // control block into the owner field before the wake, so the
            // woken thread resumes already holding it (ADR 0021). Claiming
            // it here would overwrite the live owner.
            Ok(crate::WaitOutcome::Pending) => KernelStatus::SUCCESS,
            // An infinite wait cannot time out; a service that reports
            // otherwise recorded no waiter, so taking the section is the
            // consistent reading.
            Ok(crate::WaitOutcome::Signaled | crate::WaitOutcome::TimedOut) => {
                set_field(context, base, OWNING_THREAD, owner.unwrap_or(0));
                set_field(context, base, RECURSION_COUNT, 1);
                KernelStatus::SUCCESS
            }
            Err(_) => KernelStatus::INVALID_PARAMETER,
        }
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
        let lock = field(context, base, LOCK_COUNT).wrapping_sub(1);
        set_field(context, base, RECURSION_COUNT, recursion);
        set_field(context, base, LOCK_COUNT, lock);
        if recursion == 0 {
            set_field(context, base, OWNING_THREAD, 0);
            // A lock count still at or above zero means someone is
            // waiting. Ownership is handed to exactly one waiter before it
            // wakes (ADR 0021): the release stamps that thread's control
            // block as the owner, so no third thread can slip in between
            // the release and the wake - two threads inside one section is
            // the heap corruption this structure exists to prevent.
            if lock != UNOWNED_LOCK_COUNT {
                match context.services.transfer_section_wake(base) {
                    Some(kthread) => {
                        set_field(context, base, OWNING_THREAD, kthread);
                        set_field(context, base, RECURSION_COUNT, 1);
                    }
                    // Nobody parked yet; leave the event signaled so the
                    // section's state is at least visible.
                    None => context.services.signal_dispatcher_object(base),
                }
            }
        }
        KernelStatus::SUCCESS
    }
}

/// Reads one guest `ANSI_STRING`'s bytes (`Length`@0, `Buffer`@4).
fn ansi_string_bytes(context: &KernelCallContext<'_>, pointer: u32) -> Option<Vec<u8>> {
    let length = (context.memory.read_u32(GuestVa(pointer)).ok()? & 0xFFFF) as usize;
    let buffer = context.memory.read_u32(GuestVa(pointer.wrapping_add(4))).ok()?;
    let mut bytes = vec![0_u8; length];
    if length > 0 {
        context.memory.read(GuestVa(buffer), &mut bytes).ok()?;
    }
    Some(bytes)
}

/// Compares two guest `ANSI_STRING`s for equality, returned as a BOOLEAN.
#[derive(Debug, Default, Clone, Copy)]
pub struct RtlEqualString;

impl KernelExport for RtlEqualString {
    fn ordinal(&self) -> u16 {
        crate::ordinal::RTL_EQUAL_STRING
    }

    fn name(&self) -> &'static str {
        "RtlEqualString"
    }

    fn stack_bytes(&self) -> u16 {
        12
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // RtlEqualString(String1, String2, CaseInSensitive) -> BOOLEAN in EAX.
        let (Some(first), Some(second)) = (stack_argument(context, 0), stack_argument(context, 1))
        else {
            return KernelStatus(0);
        };
        let case_insensitive = stack_argument(context, 2).unwrap_or(0) & 0xFF != 0;
        let (Some(a), Some(b)) =
            (ansi_string_bytes(context, first), ansi_string_bytes(context, second))
        else {
            return KernelStatus(0);
        };
        let equal = if case_insensitive { a.eq_ignore_ascii_case(&b) } else { a == b };
        KernelStatus(u32::from(equal))
    }
}

/// Initializes a guest `ANSI_STRING` from a zero-terminated source string.
#[derive(Debug, Default, Clone, Copy)]
pub struct RtlInitAnsiString;

impl KernelExport for RtlInitAnsiString {
    fn ordinal(&self) -> u16 {
        crate::ordinal::RTL_INIT_ANSI_STRING
    }

    fn name(&self) -> &'static str {
        "RtlInitAnsiString"
    }

    fn stack_bytes(&self) -> u16 {
        8
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // RtlInitAnsiString(DestinationString, SourceString). The destination
        // aliases the source buffer; Length is the source's strlen (capped to
        // the u16 field) and MaximumLength includes the terminator.
        let (Some(dest), Some(source)) = (stack_argument(context, 0), stack_argument(context, 1))
        else {
            return KernelStatus::INVALID_PARAMETER;
        };
        if dest == 0 {
            return KernelStatus::INVALID_PARAMETER;
        }

        let mut length = 0_u32;
        if source != 0 {
            const LIMIT: u32 = 0xFFFE;
            while length < LIMIT {
                let mut byte = [0_u8];
                match context.memory.read(GuestVa(source.wrapping_add(length)), &mut byte) {
                    Ok(()) if byte[0] != 0 => length += 1,
                    _ => break,
                }
            }
        }
        let maximum = if source == 0 { 0 } else { length + 1 };
        let packed = (length & 0xFFFF) | ((maximum & 0xFFFF) << 16);
        set_field(context, dest, 0, packed);
        set_field(context, dest, 4, source);
        // The export returns VOID; the status value is unobserved.
        KernelStatus::SUCCESS
    }
}

/// Translates one NTSTATUS to a Win32/DOS error, returned in EAX.
#[derive(Debug, Default, Clone, Copy)]
pub struct RtlNtStatusToDosError;

impl KernelExport for RtlNtStatusToDosError {
    fn ordinal(&self) -> u16 {
        crate::ordinal::RTL_NT_STATUS_TO_DOS_ERROR
    }

    fn name(&self) -> &'static str {
        "RtlNtStatusToDosError"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // The function returns a ULONG in EAX rather than an NTSTATUS; the
        // runtime places the returned value in EAX, so the DOS error rides
        // out as the "status".
        let status = stack_argument(context, 0).unwrap_or(0);
        KernelStatus(dos_error(status))
    }
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::CpuState;
    use exbawks_memory::{GuestMemory, SoftwareAddressSpace};
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, MemoryPermissions};

    use crate::{KernelServiceError, KernelServices, UnsupportedServices, WaitOutcome};

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

    /// Services that name a current thread and record waits, which is
    /// what a critical section needs to be more than a counter.
    #[derive(Debug, Default)]
    struct Threaded {
        thread: u32,
        waited: Option<u32>,
        signaled: Option<u32>,
        outcome: Option<WaitOutcome>,
    }

    impl KernelServices for Threaded {
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

        fn object_for_handle(&self, _handle: u32) -> Option<u32> {
            Some(self.thread)
        }

        fn wait_for_dispatcher_object(
            &mut self,
            address: u32,
            _timeout_ms: Option<u64>,
        ) -> Result<WaitOutcome, KernelServiceError> {
            self.waited = Some(address);
            Ok(self.outcome.unwrap_or(WaitOutcome::Pending))
        }

        fn signal_dispatcher_object(&mut self, address: u32) {
            self.signaled = Some(address);
        }
    }

    /// Calls one export with a fresh context, so the stub can be read
    /// between calls rather than borrowed for the whole test.
    fn call(
        services: &mut Threaded,
        memory: &SoftwareAddressSpace,
        cpu: &mut CpuState,
        export: &dyn KernelExport,
    ) -> KernelStatus {
        let mut context = KernelCallContext { cpu, memory, services, stop_request: None };
        export.call(&mut context)
    }

    #[test]
    fn one_thread_may_take_a_section_repeatedly() {
        let cs = 0x1100;
        let (memory, mut cpu) = context_at(cs);
        let mut services = Threaded { thread: 0x8000_1000, ..Threaded::default() };

        call(&mut services, &memory, &mut cpu, &RtlInitializeCriticalSection);
        assert_eq!(
            memory.read_u32(GuestVa(cs + LOCK_COUNT)).unwrap(),
            UNOWNED_LOCK_COUNT,
            "a free section counts from minus one"
        );

        call(&mut services, &memory, &mut cpu, &RtlEnterCriticalSection);
        call(&mut services, &memory, &mut cpu, &RtlEnterCriticalSection);
        assert_eq!(memory.read_u32(GuestVa(cs + RECURSION_COUNT)).unwrap(), 2, "recursive");
        assert_eq!(memory.read_u32(GuestVa(cs + OWNING_THREAD)).unwrap(), 0x8000_1000);
        assert!(services.waited.is_none(), "a thread never waits on itself");

        call(&mut services, &memory, &mut cpu, &RtlLeaveCriticalSection);
        assert_eq!(memory.read_u32(GuestVa(cs + RECURSION_COUNT)).unwrap(), 1);
        assert!(services.signaled.is_none(), "still held, so nobody is woken");
        call(&mut services, &memory, &mut cpu, &RtlLeaveCriticalSection);
        assert_eq!(memory.read_u32(GuestVa(cs + RECURSION_COUNT)).unwrap(), 0);
        assert_eq!(memory.read_u32(GuestVa(cs + OWNING_THREAD)).unwrap(), 0, "released");
        assert_eq!(
            memory.read_u32(GuestVa(cs + LOCK_COUNT)).unwrap(),
            UNOWNED_LOCK_COUNT,
            "and free again"
        );
        assert!(services.signaled.is_none(), "nobody was waiting");
    }

    #[test]
    fn a_second_thread_waits_and_is_woken() {
        // The defect this pins: a section that lets a second thread in
        // guards nothing. What a title guards with one is usually its
        // heap, and two threads inside an allocator at once leaves a free
        // list whose links are found broken much later and nowhere near
        // the cause.
        let cs = 0x1100;
        let (memory, mut cpu) = context_at(cs);
        let mut services = Threaded { thread: 0x8000_1000, ..Threaded::default() };
        call(&mut services, &memory, &mut cpu, &RtlInitializeCriticalSection);
        call(&mut services, &memory, &mut cpu, &RtlEnterCriticalSection);

        // A different thread asks for the same section.
        services.thread = 0x8000_2000;
        assert_eq!(
            call(&mut services, &memory, &mut cpu, &RtlEnterCriticalSection),
            KernelStatus::SUCCESS
        );
        assert_eq!(
            services.waited,
            Some(cs),
            "it waits on the section's own address, which is its event"
        );
        assert_eq!(
            memory.read_u32(GuestVa(cs + OWNING_THREAD)).unwrap(),
            0x8000_1000,
            "a parked thread must not claim a section the holder still has"
        );
        assert_eq!(
            memory.read_u32(GuestVa(cs + LOCK_COUNT)).unwrap(),
            1,
            "the holder left it at zero, so a waiter takes it to one"
        );

        // The holder releases: the waiter must be woken, or it is parked
        // for the rest of the run.
        services.thread = 0x8000_1000;
        call(&mut services, &memory, &mut cpu, &RtlLeaveCriticalSection);
        assert_eq!(services.signaled, Some(cs), "the waiter is woken");
    }

    #[test]
    fn status_maps_to_the_expected_dos_error() {
        assert_eq!(dos_error(0x0000_0000), 0, "success is NO_ERROR");
        assert_eq!(dos_error(0xC000_000D), 87, "invalid parameter");
        assert_eq!(dos_error(0xC000_0023), 122, "buffer too small");
        assert_eq!(dos_error(0x4000_0000), 0, "an informational status is NO_ERROR");
        assert_eq!(dos_error(0xC000_9999), ERROR_MR_MID_NOT_FOUND, "an unmapped failure");
    }

    #[test]
    fn dos_error_reaches_the_return_register() {
        let (memory, mut cpu) = context_at(0xC000_000D);
        let mut services = UnsupportedServices;
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };
        // The export returns the DOS error as its status; the runtime places
        // it in EAX, so asserting on the returned value is sufficient here.
        assert_eq!(RtlNtStatusToDosError.call(&mut context), KernelStatus(87));
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
