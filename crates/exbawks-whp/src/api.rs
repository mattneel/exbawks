//! Dynamically loaded Windows Hypervisor Platform entry points.
//!
//! `WinHvPlatform.dll` belongs to an optional Windows feature, so the
//! functions load at runtime through `LoadLibraryW` + `GetProcAddress`
//! (never raw-dylib: a load-time import would stop the whole CLI from
//! launching on a host without WHP). Every function returns an `HRESULT`;
//! success is non-negative.

use core::ffi::{c_char, c_void};

use thiserror::Error;

/// A WHP tier failure.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WhpError {
    /// The platform library or hypervisor is unavailable on this host.
    #[error("the Windows Hypervisor Platform is unavailable on this host")]
    Unavailable,
    /// One platform call failed with an `HRESULT`.
    #[error("{function} failed with HRESULT {hresult:#010x}")]
    Call {
        /// The failing entry point.
        function: &'static str,
        /// The failure code.
        hresult: i32,
    },
}

/// Checks one platform call's `HRESULT`.
pub(crate) fn check(function: &'static str, hresult: i32) -> Result<(), WhpError> {
    if hresult >= 0 { Ok(()) } else { Err(WhpError::Call { function, hresult }) }
}

pub(crate) type WhvCreatePartition = unsafe extern "system" fn(*mut *mut c_void) -> i32;
pub(crate) type WhvDeletePartition = unsafe extern "system" fn(*mut c_void) -> i32;
pub(crate) type WhvSetPartitionProperty =
    unsafe extern "system" fn(*mut c_void, u32, *const c_void, u32) -> i32;
pub(crate) type WhvSetupPartition = unsafe extern "system" fn(*mut c_void) -> i32;
pub(crate) type WhvCreateVirtualProcessor = unsafe extern "system" fn(*mut c_void, u32, u32) -> i32;
pub(crate) type WhvDeleteVirtualProcessor = unsafe extern "system" fn(*mut c_void, u32) -> i32;
pub(crate) type WhvMapGpaRange =
    unsafe extern "system" fn(*mut c_void, *mut c_void, u64, u64, u32) -> i32;
pub(crate) type WhvUnmapGpaRange = unsafe extern "system" fn(*mut c_void, u64, u64) -> i32;
pub(crate) type WhvSetVirtualProcessorRegisters =
    unsafe extern "system" fn(*mut c_void, u32, *const u32, u32, *const c_void) -> i32;
pub(crate) type WhvGetVirtualProcessorRegisters =
    unsafe extern "system" fn(*mut c_void, u32, *const u32, u32, *mut c_void) -> i32;
pub(crate) type WhvRunVirtualProcessor =
    unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32) -> i32;
pub(crate) type WhvCancelRunVirtualProcessor =
    unsafe extern "system" fn(*mut c_void, u32, u32) -> i32;

/// The resolved platform entry points, loaded once per process.
pub(crate) struct WhpApi {
    pub create_partition: WhvCreatePartition,
    pub delete_partition: WhvDeletePartition,
    pub set_partition_property: WhvSetPartitionProperty,
    pub setup_partition: WhvSetupPartition,
    pub create_virtual_processor: WhvCreateVirtualProcessor,
    pub delete_virtual_processor: WhvDeleteVirtualProcessor,
    pub map_gpa_range: WhvMapGpaRange,
    pub unmap_gpa_range: WhvUnmapGpaRange,
    pub set_virtual_processor_registers: WhvSetVirtualProcessorRegisters,
    pub get_virtual_processor_registers: WhvGetVirtualProcessorRegisters,
    pub run_virtual_processor: WhvRunVirtualProcessor,
    pub cancel_run_virtual_processor: WhvCancelRunVirtualProcessor,
}

// SAFETY: the struct holds only immutable code pointers into a library that
// is never unloaded (the module handle is leaked below), so sharing across
// threads is sound.
unsafe impl Sync for WhpApi {}
unsafe impl Send for WhpApi {}

/// Loads the platform library and resolves every entry point.
///
/// Returns `Unavailable` when the library or any symbol is missing. The
/// module handle is intentionally leaked so the resolved pointers stay valid
/// for the process lifetime.
pub(crate) fn load() -> Result<&'static WhpApi, WhpError> {
    use std::sync::OnceLock;
    static API: OnceLock<Option<WhpApi>> = OnceLock::new();
    API.get_or_init(load_uncached).as_ref().ok_or(WhpError::Unavailable)
}

fn load_uncached() -> Option<WhpApi> {
    // kernel32 is always linked on MSVC.
    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *const c_void;
    }

    let mut wide: Vec<u16> = "WinHvPlatform.dll".encode_utf16().collect();
    wide.push(0);
    // SAFETY: `wide` is a live null-terminated UTF-16 string. The module
    // handle is deliberately never freed, so every symbol resolved from it
    // remains callable for the process lifetime.
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        return None;
    }

    macro_rules! resolve {
        ($name:literal as $ty:ty) => {{
            // SAFETY: `module` is a live library handle and `$name` is a
            // null-terminated symbol name; the resolved pointer is transmuted
            // to the documented signature for that export.
            let symbol = unsafe { GetProcAddress(module, $name.as_ptr()) };
            if symbol.is_null() {
                return None;
            }
            // SAFETY: the export's documented signature matches `$ty`.
            unsafe { core::mem::transmute::<*const c_void, $ty>(symbol) }
        }};
    }

    Some(WhpApi {
        create_partition: resolve!(c"WHvCreatePartition" as WhvCreatePartition),
        delete_partition: resolve!(c"WHvDeletePartition" as WhvDeletePartition),
        set_partition_property: resolve!(c"WHvSetPartitionProperty" as WhvSetPartitionProperty),
        setup_partition: resolve!(c"WHvSetupPartition" as WhvSetupPartition),
        create_virtual_processor: resolve!(
            c"WHvCreateVirtualProcessor" as WhvCreateVirtualProcessor
        ),
        delete_virtual_processor: resolve!(
            c"WHvDeleteVirtualProcessor" as WhvDeleteVirtualProcessor
        ),
        map_gpa_range: resolve!(c"WHvMapGpaRange" as WhvMapGpaRange),
        unmap_gpa_range: resolve!(c"WHvUnmapGpaRange" as WhvUnmapGpaRange),
        set_virtual_processor_registers: resolve!(
            c"WHvSetVirtualProcessorRegisters" as WhvSetVirtualProcessorRegisters
        ),
        get_virtual_processor_registers: resolve!(
            c"WHvGetVirtualProcessorRegisters" as WhvGetVirtualProcessorRegisters
        ),
        run_virtual_processor: resolve!(c"WHvRunVirtualProcessor" as WhvRunVirtualProcessor),
        cancel_run_virtual_processor: resolve!(
            c"WHvCancelRunVirtualProcessor" as WhvCancelRunVirtualProcessor
        ),
    })
}
