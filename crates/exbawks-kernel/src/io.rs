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

/// Registers the Io* and symbolic-link-object exports.
pub(crate) fn register_io_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(IoCreateSymbolicLink)?;
    registry.register(IoDeleteSymbolicLink)?;
    registry.register(NtOpenSymbolicLinkObject)?;
    registry.register(NtQuerySymbolicLinkObject)?;
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

/// Opens a handle to an existing symbolic-link object.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtOpenSymbolicLinkObject;

impl KernelExport for NtOpenSymbolicLinkObject {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_OPEN_SYMBOLIC_LINK_OBJECT
    }

    fn name(&self) -> &'static str {
        "NtOpenSymbolicLinkObject"
    }

    fn stack_bytes(&self) -> u16 {
        8
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtOpenSymbolicLinkObject(LinkHandle, ObjectAttributes). The name
        // comes from OBJECT_ATTRIBUTES.ObjectName (PANSI_STRING at +4).
        let (Some(handle_out), Some(attributes)) =
            (stack_argument(context, 0), stack_argument(context, 1))
        else {
            return KernelStatus::INVALID_PARAMETER;
        };
        if handle_out == 0 || attributes == 0 {
            return KernelStatus::INVALID_PARAMETER;
        }
        let Ok(name_pointer) = context.memory.read_u32(GuestVa(attributes + 4)) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let Some(name) = ansi_string(context, name_pointer) else {
            return KernelStatus::OBJECT_NAME_NOT_FOUND;
        };
        match context.services.open_symbolic_link(&name) {
            Ok(handle) => {
                let _ = context.memory.write_u32(GuestVa(handle_out), handle);
                KernelStatus::SUCCESS
            }
            Err(_) => KernelStatus::OBJECT_NAME_NOT_FOUND,
        }
    }
}

/// Reads the target string of an open symbolic-link handle.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtQuerySymbolicLinkObject;

impl KernelExport for NtQuerySymbolicLinkObject {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_QUERY_SYMBOLIC_LINK_OBJECT
    }

    fn name(&self) -> &'static str {
        "NtQuerySymbolicLinkObject"
    }

    fn stack_bytes(&self) -> u16 {
        12
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtQuerySymbolicLinkObject(LinkHandle, LinkTarget: PANSI_STRING,
        //                           ReturnedLength: PULONG optional).
        let (Some(handle), Some(target_out)) =
            (stack_argument(context, 0), stack_argument(context, 1))
        else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let returned_out = stack_argument(context, 2).unwrap_or(0);
        let target = match context.services.query_symbolic_link(handle) {
            Ok(target) => target,
            Err(_) => return KernelStatus::INVALID_HANDLE,
        };
        if returned_out != 0 {
            let _ = context.memory.write_u32(GuestVa(returned_out), target.len() as u32);
        }
        // Fill the caller's ANSI_STRING: Length@0 (u16), MaximumLength@2,
        // Buffer@4. A short buffer takes a truncated copy and reports it.
        let Ok(packed) = context.memory.read_u32(GuestVa(target_out)) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let maximum = (packed >> 16) & 0xFFFF;
        let Ok(buffer) = context.memory.read_u32(GuestVa(target_out + 4)) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let copy = target.len().min(maximum as usize);
        if buffer != 0 && copy > 0 {
            let _ = context.memory.write(GuestVa(buffer), &target.as_bytes()[..copy]);
        }
        let repacked = (copy as u32 & 0xFFFF) | (maximum << 16);
        let _ = context.memory.write_u32(GuestVa(target_out), repacked);
        if copy < target.len() {
            return KernelStatus::BUFFER_TOO_SMALL;
        }
        KernelStatus::SUCCESS
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
