//! Xe* image exports.
//!
//! Titles demand-load their own XBE sections (`XeLoadSection`) around large
//! subsystems (D3D, sound, video). The loader maps the complete contiguous
//! section union up front (ADR 0007), so every section's bytes are already
//! resident; these exports maintain the guest-visible reference counts on
//! the section header and report success.

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
use crate::{KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus};

/// `XBEIMAGE_SECTION` field offsets: the section reference count and the
/// shared head/tail page reference-count pointers.
const SECTION_REF_COUNT: u32 = 0x18;
const HEAD_REF_POINTER: u32 = 0x1C;
const TAIL_REF_POINTER: u32 = 0x20;

/// Registers the Xe* image exports.
pub(crate) fn register_xe_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(XeLoadSection)?;
    registry.register(XeUnloadSection)?;
    Ok(())
}

/// Adjusts one u16 shared page reference count through its pointer field.
fn adjust_shared_count(context: &mut KernelCallContext<'_>, section: u32, field: u32, delta: i32) {
    let Ok(pointer) = context.memory.read_u32(GuestVa(section.wrapping_add(field))) else {
        return;
    };
    if pointer == 0 {
        return;
    }
    let mut bytes = [0_u8; 2];
    if context.memory.read(GuestVa(pointer), &mut bytes).is_err() {
        return;
    }
    let value = u16::from_le_bytes(bytes);
    let adjusted = if delta > 0 { value.saturating_add(1) } else { value.saturating_sub(1) };
    let _ = context.memory.write(GuestVa(pointer), &adjusted.to_le_bytes());
}

/// Adjusts a section's reference counts and reports the outcome.
fn adjust_section(context: &mut KernelCallContext<'_>, delta: i32) -> KernelStatus {
    let Some(section) = stack_argument(context, 0).filter(|pointer| *pointer != 0) else {
        return KernelStatus::INVALID_PARAMETER;
    };
    let Ok(count) = context.memory.read_u32(GuestVa(section.wrapping_add(SECTION_REF_COUNT)))
    else {
        return KernelStatus::INVALID_PARAMETER;
    };
    let adjusted = if delta > 0 { count.saturating_add(1) } else { count.saturating_sub(1) };
    let _ = context.memory.write_u32(GuestVa(section.wrapping_add(SECTION_REF_COUNT)), adjusted);
    adjust_shared_count(context, section, HEAD_REF_POINTER, delta);
    adjust_shared_count(context, section, TAIL_REF_POINTER, delta);
    KernelStatus::SUCCESS
}

/// Loads (references) one XBE section already resident under ADR 0007.
#[derive(Debug, Default, Clone, Copy)]
pub struct XeLoadSection;

impl KernelExport for XeLoadSection {
    fn ordinal(&self) -> u16 {
        crate::ordinal::XE_LOAD_SECTION
    }

    fn name(&self) -> &'static str {
        "XeLoadSection"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        adjust_section(context, 1)
    }
}

/// Unloads (dereferences) one XBE section; the bytes stay resident.
#[derive(Debug, Default, Clone, Copy)]
pub struct XeUnloadSection;

impl KernelExport for XeUnloadSection {
    fn ordinal(&self) -> u16 {
        crate::ordinal::XE_UNLOAD_SECTION
    }

    fn name(&self) -> &'static str {
        "XeUnloadSection"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        adjust_section(context, -1)
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
    fn load_section_bumps_the_reference_counts() {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 4 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        // Section header at 0x3000; head/tail shared u16 counts at 0x3100/2.
        memory.write_u32(GuestVa(0x3000 + SECTION_REF_COUNT), 0).expect("write");
        memory.write_u32(GuestVa(0x3000 + HEAD_REF_POINTER), 0x3100).expect("write");
        memory.write_u32(GuestVa(0x3000 + TAIL_REF_POINTER), 0x3102).expect("write");
        memory.write_u32(GuestVa(0x2004), 0x3000).expect("write");
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut services = UnsupportedServices;
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut services,
            stop_request: None,
        };

        assert_eq!(XeLoadSection.call(&mut context), KernelStatus::SUCCESS);
        assert_eq!(memory.read_u32(GuestVa(0x3000 + SECTION_REF_COUNT)).unwrap(), 1);
        // The packed u16 pair at 0x3100 both incremented.
        assert_eq!(memory.read_u32(GuestVa(0x3100)).unwrap(), 0x0001_0001);

        assert_eq!(XeUnloadSection.call(&mut context), KernelStatus::SUCCESS);
        assert_eq!(memory.read_u32(GuestVa(0x3000 + SECTION_REF_COUNT)).unwrap(), 0);
        assert_eq!(memory.read_u32(GuestVa(0x3100)).unwrap(), 0);
    }
}
