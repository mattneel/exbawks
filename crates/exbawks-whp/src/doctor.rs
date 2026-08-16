//! WHP capability probing — the honest first question of the tier: can this
//! host create a partition at all?
//!
//! The Windows Hypervisor Platform DLLs may be absent when the optional
//! feature is off, so the probe loads `WinHvPlatform.dll` dynamically rather
//! than as a load-time import; every other Exbawks command still runs on a
//! host without WHP.

use serde::{Deserialize, Serialize};

/// Whether the Windows Hypervisor Platform is usable on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhpAvailability {
    /// True when the host compiles for the Windows x86-64 runtime target.
    pub supported_target: bool,
    /// True when `WinHvPlatform.dll` loaded (the optional feature's library
    /// is installed).
    pub library_present: bool,
    /// True when `WHvGetCapability` reports the hypervisor is present.
    ///
    /// This is `false` when the Windows Hypervisor Platform optional feature
    /// is installed but disabled, and always `false` off the supported
    /// target or when the library is absent.
    pub hypervisor_present: bool,
}

impl WhpAvailability {
    /// True when the tier can create a partition on this host.
    #[must_use]
    pub const fn usable(self) -> bool {
        self.supported_target && self.library_present && self.hypervisor_present
    }
}

/// Probes whether WHP can back the execution tier on this host.
#[must_use]
pub fn probe_whp() -> WhpAvailability {
    let (library_present, hypervisor_present) = probe();
    WhpAvailability {
        supported_target: cfg!(all(windows, target_arch = "x86_64")),
        library_present,
        hypervisor_present,
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn probe() -> (bool, bool) {
    use core::ffi::{c_char, c_void};

    // kernel32 is always linked on MSVC, so these resolve without a hard
    // dependency on the hypervisor libraries.
    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *const c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
    }

    // `WHvCapabilityCodeHypervisorPresent`; the output is a 4-byte BOOL.
    const WHV_CAPABILITY_CODE_HYPERVISOR_PRESENT: u32 = 0x0000_0000;
    type WhvGetCapability = unsafe extern "system" fn(u32, *mut c_void, u32, *mut u32) -> i32;

    let mut wide: Vec<u16> = "WinHvPlatform.dll".encode_utf16().collect();
    wide.push(0);

    // SAFETY: `wide` is a live null-terminated UTF-16 string for the call.
    // The module handle, when non-null, is freed before return, and the
    // resolved symbol is transmuted to its documented signature and called
    // with a live 4-byte output and a live u32 written-count.
    unsafe {
        let module = LoadLibraryW(wide.as_ptr());
        if module.is_null() {
            return (false, false);
        }
        let symbol = GetProcAddress(module, c"WHvGetCapability".as_ptr());
        let present = if symbol.is_null() {
            false
        } else {
            let get_capability = core::mem::transmute::<*const c_void, WhvGetCapability>(symbol);
            let mut value: i32 = 0;
            let mut written: u32 = 0;
            let result = get_capability(
                WHV_CAPABILITY_CODE_HYPERVISOR_PRESENT,
                (&raw mut value).cast::<c_void>(),
                4,
                &raw mut written,
            );
            // The call returns an HRESULT; success is non-negative. WHP
            // reports absence as S_OK with a false payload.
            result >= 0 && written >= 4 && value != 0
        };
        FreeLibrary(module);
        (true, present)
    }
}

#[cfg(not(all(windows, target_arch = "x86_64")))]
const fn probe() -> (bool, bool) {
    (false, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_a_consistent_target_flag() {
        let availability = probe_whp();
        assert_eq!(availability.supported_target, cfg!(all(windows, target_arch = "x86_64")));
        // The hypervisor can never be present without its library or target.
        if !availability.library_present {
            assert!(!availability.hypervisor_present);
        }
        if availability.usable() {
            assert!(availability.hypervisor_present);
        }
    }
}
