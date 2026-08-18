//! Reading a game controller from the host, over raw HID.
//!
//! ADR 0019 keeps the guest-facing device model pure and portable and puts
//! the host device here, with the rest of the operating system calls. What
//! leaves this module is a report's bytes and nothing else: no handle, no
//! pointer, and no knowledge of which controller produced them.

use std::ffi::c_void;

use crate::error::PlatformError;

/// A handle the operating system owns.
type Handle = *mut c_void;

/// The value a failed handle-returning call reports.
const INVALID_HANDLE: isize = -1;

const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;

/// `SetupDiGetClassDevsW` flags: present devices exposing an interface.
const DIGCF_PRESENT: u32 = 0x0000_0002;
const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;

/// A globally unique identifier, as the setup interface takes it.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// One enumerated device interface.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct DeviceInterfaceData {
    size: u32,
    class: Guid,
    flags: u32,
    reserved: usize,
}

/// A controller's identity, as the driver reports it.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct HidAttributes {
    size: u32,
    vendor: u16,
    product: u16,
    version: u16,
}

#[link(name = "hid")]
unsafe extern "system" {
    fn HidD_GetHidGuid(guid: *mut Guid);
    fn HidD_GetAttributes(device: Handle, attributes: *mut HidAttributes) -> u8;
}

#[link(name = "setupapi")]
unsafe extern "system" {
    fn SetupDiGetClassDevsW(
        class: *const Guid,
        enumerator: *const u16,
        parent: *mut c_void,
        flags: u32,
    ) -> Handle;
    fn SetupDiEnumDeviceInterfaces(
        devices: Handle,
        info: *const c_void,
        class: *const Guid,
        index: u32,
        interface: *mut DeviceInterfaceData,
    ) -> i32;
    fn SetupDiGetDeviceInterfaceDetailW(
        devices: Handle,
        interface: *const DeviceInterfaceData,
        detail: *mut u8,
        detail_size: u32,
        required: *mut u32,
        info: *mut c_void,
    ) -> i32;
    fn SetupDiDestroyDeviceInfoList(devices: Handle) -> i32;
}

#[link(name = "Kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn ReadFile(
        handle: Handle,
        buffer: *mut u8,
        count: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CancelIoEx(handle: Handle, overlapped: *mut c_void) -> i32;
}

/// An open controller, closed when it goes out of scope.
#[derive(Debug)]
pub struct HidDevice {
    handle: Handle,
    vendor: u16,
    product: u16,
}

// SAFETY: the handle is owned solely by this value, every use goes through
// `&mut self`, and the operating system permits a handle to be used from
// any thread.
unsafe impl Send for HidDevice {}

impl HidDevice {
    /// The identity the driver reports for this device.
    #[must_use]
    pub fn identity(&self) -> (u16, u16) {
        (self.vendor, self.product)
    }

    /// Reads one input report, returning how many bytes it filled.
    ///
    /// The read blocks until the device sends something, which for a
    /// controller is on every state change and at its own polling rate.
    pub fn read(&mut self, report: &mut [u8]) -> Result<usize, PlatformError> {
        let mut read = 0_u32;
        let count = u32::try_from(report.len()).unwrap_or(u32::MAX);
        // The handle is opened synchronously, so no overlapped structure
        // is supplied and the call returns when the report arrives.
        // SAFETY: `handle` is an open device this value owns, `report` is a
        // caller-owned slice of `count` bytes outliving the call, and
        // `read` is a local only this call writes.
        let ok = unsafe {
            ReadFile(self.handle, report.as_mut_ptr(), count, &raw mut read, std::ptr::null_mut())
        };
        if ok == 0 {
            return Err(PlatformError::Win32 { operation: "ReadFile", code: last_error() });
        }
        Ok(read as usize)
    }

    /// Cancels a read another thread is blocked in, so it can be dropped.
    pub fn cancel(&self) {
        // SAFETY: `handle` is open for the lifetime of this value, and
        // cancelling with a null overlapped pointer asks for every pending
        // operation on it, which is exactly what a shutdown wants.
        unsafe {
            CancelIoEx(self.handle, std::ptr::null_mut());
        }
    }
}

impl Drop for HidDevice {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `open` and is closed once, here,
        // because nothing else owns a copy of it.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// The last error the operating system recorded.
fn last_error() -> u32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32
}

/// Opens the first attached controller matching `wanted`, if any.
///
/// `wanted` is a list of `(vendor, product)` pairs in preference order.
/// Returns `None` when none of them is attached, which is not an error: a
/// run without a controller is the ordinary case.
pub fn open_controller(wanted: &[(u16, u16)]) -> Result<Option<HidDevice>, PlatformError> {
    let mut class = Guid::default();
    // SAFETY: the callee fills a caller-owned value that lives across the
    // call and writes nothing else.
    unsafe { HidD_GetHidGuid(&raw mut class) };

    // SAFETY: `class` outlives the call; the enumerator and parent are
    // null because every present interface of that class is wanted.
    let devices = unsafe {
        SetupDiGetClassDevsW(
            &raw const class,
            std::ptr::null(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if devices as isize == INVALID_HANDLE {
        return Err(PlatformError::Win32 { operation: "SetupDiGetClassDevsW", code: last_error() });
    }

    let found = enumerate(devices, &class, wanted);
    // SAFETY: `devices` came from the call above, is still open, and is
    // released exactly once here whatever the enumeration found.
    unsafe { SetupDiDestroyDeviceInfoList(devices) };
    found
}

/// Walks the interfaces of one device list, opening the first match.
fn enumerate(
    devices: Handle,
    class: &Guid,
    wanted: &[(u16, u16)],
) -> Result<Option<HidDevice>, PlatformError> {
    /// A device with more interfaces than this is not a controller.
    const MAX_INTERFACES: u32 = 512;
    /// The detail buffer, large enough for any interface path.
    const DETAIL_BYTES: usize = 1024;

    for index in 0..MAX_INTERFACES {
        let mut interface = DeviceInterfaceData {
            size: u32::try_from(size_of::<DeviceInterfaceData>()).unwrap_or(0),
            ..DeviceInterfaceData::default()
        };
        // SAFETY: `devices` is an open list, `class` and `interface` are
        // caller-owned and outlive the call, and a null info pointer asks
        // for the interfaces of the whole list.
        let more = unsafe {
            SetupDiEnumDeviceInterfaces(
                devices,
                std::ptr::null(),
                &raw const *class,
                index,
                &raw mut interface,
            )
        };
        if more == 0 {
            break;
        }

        let mut detail = [0_u8; DETAIL_BYTES];
        // The detail structure begins with its own size, which on a 64-bit
        // host counts the size field and one aligned character.
        let header = 8_u32;
        detail[..4].copy_from_slice(&header.to_le_bytes());
        let mut required = 0_u32;
        // SAFETY: `detail` is a caller-owned buffer of `DETAIL_BYTES` and
        // the size passed is exactly that; `required` is a local; the info
        // pointer is null because only the path is wanted.
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                devices,
                &raw const interface,
                detail.as_mut_ptr(),
                u32::try_from(DETAIL_BYTES).unwrap_or(0),
                &raw mut required,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            continue;
        }

        // The path follows the size field, as wide characters.
        let path: Vec<u16> = detail[4..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .chain(std::iter::once(0))
            .collect();
        if path.len() <= 1 {
            continue;
        }

        // SAFETY: `path` is a caller-owned, nul-terminated wide string that
        // outlives the call. The device is opened for reading only and
        // shared, so opening it does not disturb anything else using it.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle as isize == INVALID_HANDLE {
            continue;
        }

        let mut attributes = HidAttributes {
            size: u32::try_from(size_of::<HidAttributes>()).unwrap_or(0),
            ..HidAttributes::default()
        };
        // SAFETY: `handle` is open and `attributes` is a caller-owned value
        // that outlives the call.
        let described = unsafe { HidD_GetAttributes(handle, &raw mut attributes) };
        let matched = described != 0 && wanted.contains(&(attributes.vendor, attributes.product));
        if matched {
            return Ok(Some(HidDevice {
                handle,
                vendor: attributes.vendor,
                product: attributes.product,
            }));
        }
        // SAFETY: this handle was opened just above, is not being returned,
        // and nothing else holds a copy of it.
        unsafe { CloseHandle(handle) };
    }
    Ok(None)
}

/// The controllers this reader knows how to open, in preference order.
///
/// Only the identity is listed here; what the bytes mean is the guest-side
/// model's business, not the host's.
#[must_use]
pub fn known_controllers() -> Vec<(u16, u16)> {
    vec![
        // Sony DualSense, and the edge model, over USB.
        (0x054C, 0x0CE6),
        (0x054C, 0x0DF2),
        // Sony DualShock 4, both revisions.
        (0x054C, 0x05C4),
        (0x054C, 0x09CC),
    ]
}
