//! Ex* executive exports.
//!
//! The Xbox stores per-console configuration in an EEPROM (language, video
//! and audio flags, region). `ExQueryNonVolatileSetting` reads one setting;
//! titles query it during startup to pick a video mode and language before
//! they present anything. There is no real EEPROM here, so the HLE returns
//! canned NTSC-U defaults — a synthetic console profile, never real console
//! data (ADR 0010 keeps guest-visible keys and identifiers zeroed).

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
use crate::{KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus};

/// `REG_DWORD` value type, the type of every setting the HLE returns today.
const REG_DWORD: u32 = 4;

/// `AV_STANDARD_NTSC_M`, the factory AV region for an NTSC-U console.
const AV_STANDARD_NTSC_M: u32 = 0x0000_0100;
/// The North American game region bit (`XC_GAME_REGION_NA`).
const GAME_REGION_NA: u32 = 0x0000_0001;

/// Registers the Ex* executive exports.
pub(crate) fn register_ex_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(ExQueryNonVolatileSetting)?;
    Ok(())
}

/// Returns the canned DWORD value for one EEPROM setting index.
///
/// `None` selects the benign fallback (a zeroed DWORD) so an unrecognized
/// query still succeeds and boot proceeds; every value here is a synthetic
/// default, not a real console setting.
fn setting_value(index: u32) -> Option<u32> {
    match index {
        0x07 => Some(1),                    // XC_LANGUAGE: English
        0x08 => Some(0),                    // XC_VIDEO: no flags (NTSC 4:3)
        0x09 => Some(0),                    // XC_AUDIO: stereo
        0x0A => Some(0),                    // XC_P_CONTROL_GAMES: unrestricted
        0x0C => Some(0),                    // XC_P_CONTROL_MOVIES: unrestricted
        0x11 => Some(0),                    // XC_MISC
        0x12 => Some(1),                    // XC_DVD_REGION: region 1 (NTSC-U)
        0x0103 => Some(AV_STANDARD_NTSC_M), // XC_FACTORY_AV_REGION
        0x0104 => Some(GAME_REGION_NA),     // XC_FACTORY_GAME_REGION
        _ => None,
    }
}

/// Reads one non-volatile (EEPROM) setting.
///
/// `ExQueryNonVolatileSetting(ValueIndex, Type, Value, ValueLength,
/// ResultLength)` writes the value type and length unconditionally, then the
/// value when the caller's buffer is large enough; a short buffer reports
/// `BUFFER_TOO_SMALL` with the required length so the caller can retry.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExQueryNonVolatileSetting;

impl KernelExport for ExQueryNonVolatileSetting {
    fn ordinal(&self) -> u16 {
        crate::ordinal::EX_QUERY_NON_VOLATILE_SETTING
    }

    fn name(&self) -> &'static str {
        "ExQueryNonVolatileSetting"
    }

    fn stack_bytes(&self) -> u16 {
        20
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let (Some(index), Some(type_ptr), Some(value_ptr), Some(value_len)) = (
            stack_argument(context, 0),
            stack_argument(context, 1),
            stack_argument(context, 2),
            stack_argument(context, 3),
        ) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let result_len_ptr = stack_argument(context, 4).unwrap_or(0);

        // Every setting the HLE knows is a 4-byte DWORD; an unknown index
        // falls back to a zeroed DWORD so boot proceeds.
        let value = setting_value(index).unwrap_or(0);
        let length = 4_u32;

        if type_ptr != 0 {
            let _ = context.memory.write_u32(GuestVa(type_ptr), REG_DWORD);
        }
        if result_len_ptr != 0 {
            let _ = context.memory.write_u32(GuestVa(result_len_ptr), length);
        }
        if value_len < length {
            return KernelStatus::BUFFER_TOO_SMALL;
        }
        if value_ptr != 0 {
            let _ = context.memory.write_u32(GuestVa(value_ptr), value);
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

    /// Builds a memory space with one call frame's arguments laid out above
    /// the return slot, then runs the export and returns the guest memory.
    fn query(index: u32, value_len: u32) -> (KernelStatus, SoftwareAddressSpace) {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 4 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        // esp = 0x2000; [esp]=return, [esp+4..]=args. Output pointers land in
        // the mapped scratch page at 0x3000/0x3010/0x3020.
        let args = [index, 0x3000, 0x3010, value_len, 0x3020];
        for (slot, value) in args.iter().enumerate() {
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
        let status = ExQueryNonVolatileSetting.call(&mut context);
        (status, memory)
    }

    #[test]
    fn known_setting_writes_type_value_and_length() {
        let (status, memory) = query(0x07, 4);
        assert_eq!(status, KernelStatus::SUCCESS);
        assert_eq!(memory.read_u32(GuestVa(0x3000)).unwrap(), REG_DWORD);
        assert_eq!(memory.read_u32(GuestVa(0x3010)).unwrap(), 1, "language is English");
        assert_eq!(memory.read_u32(GuestVa(0x3020)).unwrap(), 4, "result length is the DWORD size");
    }

    #[test]
    fn unknown_setting_succeeds_with_a_zeroed_dword() {
        let (status, memory) = query(0x4242, 4);
        assert_eq!(status, KernelStatus::SUCCESS);
        assert_eq!(memory.read_u32(GuestVa(0x3010)).unwrap(), 0);
    }

    #[test]
    fn short_buffer_reports_the_required_length() {
        let (status, memory) = query(0x07, 2);
        assert_eq!(status, KernelStatus::BUFFER_TOO_SMALL);
        // The caller still learns the type and required size.
        assert_eq!(memory.read_u32(GuestVa(0x3000)).unwrap(), REG_DWORD);
        assert_eq!(memory.read_u32(GuestVa(0x3020)).unwrap(), 4);
    }
}
