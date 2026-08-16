//! Mutant (mutex) objects.
//!
//! Titles guard shared state with mutants far more often than they contend
//! for them: the owning thread reacquires recursively and releases in
//! matching pairs. Ownership is tracked by the emulator's thread table, so
//! a wait on a mutant another thread holds parks exactly like an event wait
//! (ADR 0017) instead of livelocking.

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
use crate::{KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus};

/// Registers the mutant exports.
pub(crate) fn register_mutant_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(NtCreateMutant)?;
    registry.register(NtReleaseMutant)?;
    Ok(())
}

/// Creates a mutant object, optionally owned by the caller.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtCreateMutant;

impl KernelExport for NtCreateMutant {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_CREATE_MUTANT
    }

    fn name(&self) -> &'static str {
        "NtCreateMutant"
    }

    fn stack_bytes(&self) -> u16 {
        12
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtCreateMutant(MutantHandle, ObjectAttributes, InitialOwner).
        let (Some(handle_out), Some(initial_owner)) =
            (stack_argument(context, 0), stack_argument(context, 2))
        else {
            return KernelStatus::INVALID_PARAMETER;
        };
        if handle_out == 0 {
            return KernelStatus::INVALID_PARAMETER;
        }
        match context.services.create_mutant(initial_owner & 0xFF != 0) {
            Ok(handle) => {
                let _ = context.memory.write_u32(GuestVa(handle_out), handle);
                KernelStatus::SUCCESS
            }
            Err(_) => KernelStatus::INSUFFICIENT_RESOURCES,
        }
    }
}

/// Releases one level of ownership of a mutant.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtReleaseMutant;

impl KernelExport for NtReleaseMutant {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_RELEASE_MUTANT
    }

    fn name(&self) -> &'static str {
        "NtReleaseMutant"
    }

    fn stack_bytes(&self) -> u16 {
        8
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtReleaseMutant(MutantHandle, PreviousCount).
        let Some(handle) = stack_argument(context, 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let previous_out = stack_argument(context, 1).unwrap_or(0);
        match context.services.release_mutant(handle) {
            Ok(previous) => {
                if previous_out != 0 {
                    let _ = context.memory.write_u32(GuestVa(previous_out), previous);
                }
                KernelStatus::SUCCESS
            }
            Err(crate::KernelServiceError::AccessDenied) => {
                // STATUS_MUTANT_NOT_OWNED: releasing a mutant this thread
                // does not hold is the caller's bug, not ours.
                KernelStatus(0xC000_0046)
            }
            Err(_) => KernelStatus::INVALID_HANDLE,
        }
    }
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::CpuState;
    use exbawks_memory::{GuestMemory, SoftwareAddressSpace};
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, MemoryPermissions};

    use crate::{KernelServiceError, KernelServices};

    use super::*;

    /// A services stub with one mutant.
    #[derive(Debug, Default)]
    struct MutantServices {
        owned: bool,
        created_owned: Option<bool>,
        released: Option<u32>,
    }

    impl KernelServices for MutantServices {
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

        fn create_mutant(&mut self, initially_owned: bool) -> Result<u32, KernelServiceError> {
            self.created_owned = Some(initially_owned);
            Ok(0xB000)
        }

        fn release_mutant(&mut self, handle: u32) -> Result<u32, KernelServiceError> {
            self.released = Some(handle);
            if self.owned { Ok(0) } else { Err(KernelServiceError::AccessDenied) }
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
    fn creating_a_mutant_writes_its_handle() {
        let memory = mapped_memory();
        let mut services = MutantServices::default();
        assert_eq!(
            call(&NtCreateMutant, &[0x3000, 0, 1], &mut services, &memory),
            KernelStatus::SUCCESS
        );
        assert_eq!(memory.read_u32(GuestVa(0x3000)).expect("handle out"), 0xB000);
        assert_eq!(services.created_owned, Some(true), "the caller takes initial ownership");
    }

    #[test]
    fn a_null_handle_pointer_is_rejected() {
        let memory = mapped_memory();
        let mut services = MutantServices::default();
        assert_eq!(
            call(&NtCreateMutant, &[0, 0, 0], &mut services, &memory),
            KernelStatus::INVALID_PARAMETER
        );
    }

    #[test]
    fn releasing_an_owned_mutant_reports_the_previous_count() {
        let memory = mapped_memory();
        let mut services = MutantServices { owned: true, ..Default::default() };
        memory.write_u32(GuestVa(0x3010), 0xFFFF_FFFF).expect("scratch");
        assert_eq!(
            call(&NtReleaseMutant, &[0xB000, 0x3010], &mut services, &memory),
            KernelStatus::SUCCESS
        );
        assert_eq!(memory.read_u32(GuestVa(0x3010)).expect("previous count"), 0);
        assert_eq!(services.released, Some(0xB000));
    }

    #[test]
    fn releasing_an_unowned_mutant_reports_not_owned() {
        let memory = mapped_memory();
        let mut services = MutantServices::default();
        assert_eq!(
            call(&NtReleaseMutant, &[0xB000, 0], &mut services, &memory),
            KernelStatus(0xC000_0046)
        );
    }
}
