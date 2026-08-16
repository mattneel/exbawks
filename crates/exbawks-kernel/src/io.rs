//! Io* exports (HLE-005 slice: object-namespace symbolic links).
//!
//! Titles mount their drive letters at startup: `IoCreateSymbolicLink` links
//! `\??\D:` to the disc device and the title/user data letters to hard-disk
//! paths. The links live in the emulator's file device, which rewrites a
//! matching prefix during path resolution, so subsequent opens through a
//! drive letter reach the right mount.

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
use crate::{KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus};

/// The longest ANSI string the exports read from guest memory.
const MAX_STRING_BYTES: usize = 512;

/// Registers the Io* symbolic-link exports.
pub(crate) fn register_io_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(IoCreateSymbolicLink)?;
    registry.register(IoDeleteSymbolicLink)?;
    Ok(())
}

/// Reads one guest `ANSI_STRING` (`Length`@0, `MaximumLength`@2, `Buffer`@4).
fn ansi_string(context: &KernelCallContext<'_>, pointer: u32) -> Option<String> {
    if pointer == 0 {
        return None;
    }
    let length = (context.memory.read_u32(GuestVa(pointer)).ok()? & 0xFFFF) as usize;
    let buffer = context.memory.read_u32(GuestVa(pointer + 4)).ok()?;
    if buffer == 0 || length == 0 {
        return None;
    }
    let mut bytes = vec![0_u8; length.min(MAX_STRING_BYTES)];
    context.memory.read(GuestVa(buffer), &mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Creates an object-namespace symbolic link (drive-letter mounting).
#[derive(Debug, Default, Clone, Copy)]
pub struct IoCreateSymbolicLink;

impl KernelExport for IoCreateSymbolicLink {
    fn ordinal(&self) -> u16 {
        crate::ordinal::IO_CREATE_SYMBOLIC_LINK
    }

    fn name(&self) -> &'static str {
        "IoCreateSymbolicLink"
    }

    fn stack_bytes(&self) -> u16 {
        8
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // IoCreateSymbolicLink(SymbolicLinkName, DeviceName), both
        // PANSI_STRING.
        let (Some(name_ptr), Some(target_ptr)) =
            (stack_argument(context, 0), stack_argument(context, 1))
        else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let (Some(name), Some(target)) =
            (ansi_string(context, name_ptr), ansi_string(context, target_ptr))
        else {
            return KernelStatus::INVALID_PARAMETER;
        };
        match context.services.create_symbolic_link(name, target) {
            Ok(()) => KernelStatus::SUCCESS,
            Err(_) => KernelStatus::INSUFFICIENT_RESOURCES,
        }
    }
}

/// Removes an object-namespace symbolic link.
#[derive(Debug, Default, Clone, Copy)]
pub struct IoDeleteSymbolicLink;

impl KernelExport for IoDeleteSymbolicLink {
    fn ordinal(&self) -> u16 {
        crate::ordinal::IO_DELETE_SYMBOLIC_LINK
    }

    fn name(&self) -> &'static str {
        "IoDeleteSymbolicLink"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let Some(name_ptr) = stack_argument(context, 0) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let Some(name) = ansi_string(context, name_ptr) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        if context.services.delete_symbolic_link(&name) {
            KernelStatus::SUCCESS
        } else {
            KernelStatus::OBJECT_NAME_NOT_FOUND
        }
    }
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::CpuState;
    use exbawks_memory::{GuestMemory, SoftwareAddressSpace};
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, MemoryPermissions};

    use crate::KernelServiceError;

    use super::*;

    /// A service that records created links.
    #[derive(Default)]
    struct LinkFake {
        created: Option<(String, String)>,
    }

    impl crate::KernelServices for LinkFake {
        fn create_thread(
            &mut self,
            _request: crate::ThreadCreateRequest,
        ) -> Result<crate::ThreadCreated, KernelServiceError> {
            Err(KernelServiceError::Unsupported)
        }

        fn exit_current_thread(&mut self, _status: u32) {}

        fn close_handle(&mut self, _handle: u32) -> bool {
            false
        }

        fn allocate_virtual_memory(
            &mut self,
            _request: crate::VirtualAllocRequest,
        ) -> Result<crate::VirtualAllocation, KernelServiceError> {
            Err(KernelServiceError::Unsupported)
        }

        fn create_symbolic_link(
            &mut self,
            name: String,
            target: String,
        ) -> Result<(), KernelServiceError> {
            self.created = Some((name, target));
            Ok(())
        }
    }

    /// Writes one ANSI_STRING (header at `header`, text at `text`).
    fn write_ansi(memory: &SoftwareAddressSpace, header: u32, text: u32, value: &str) {
        let packed = (value.len() as u32) | ((value.len() as u32) << 16);
        memory.write_u32(GuestVa(header), packed).expect("write");
        memory.write_u32(GuestVa(header + 4), text).expect("write");
        memory.write(GuestVa(text), value.as_bytes()).expect("write");
    }

    #[test]
    fn create_link_reads_both_ansi_strings() {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 4 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        write_ansi(&memory, 0x3000, 0x3100, "\\??\\D:");
        write_ansi(&memory, 0x3010, 0x3200, "\\Device\\CdRom0");
        // Stack: [esp]=return, then SymbolicLinkName, DeviceName.
        memory.write_u32(GuestVa(0x2004), 0x3000).expect("write");
        memory.write_u32(GuestVa(0x2008), 0x3010).expect("write");
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut services = LinkFake::default();
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };

        assert_eq!(IoCreateSymbolicLink.call(&mut context), KernelStatus::SUCCESS);
        assert_eq!(services.created, Some(("\\??\\D:".to_owned(), "\\Device\\CdRom0".to_owned())));
    }
}
