use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use crate::PlatformError;

use super::super::PageProtection;

const PAGE_NOACCESS: u32 = 0x01;
const PAGE_READONLY: u32 = 0x02;
const PAGE_READWRITE: u32 = 0x04;
const PAGE_EXECUTE_READ: u32 = 0x20;
const PAGE_EXECUTE_READWRITE: u32 = 0x40;

const MEM_RESERVE: u32 = 0x0000_2000;
const MEM_REPLACE_PLACEHOLDER: u32 = 0x0000_4000;
const MEM_RESERVE_PLACEHOLDER: u32 = 0x0004_0000;
const MEM_RELEASE: u32 = 0x0000_8000;
const MEM_PRESERVE_PLACEHOLDER: u32 = 0x0000_0002;

const INVALID_HANDLE_VALUE: *mut c_void = -1_isize as *mut c_void;

type Handle = *mut c_void;

#[link(name = "Kernel32")]
unsafe extern "system" {
    fn CreateFileMappingW(
        file: Handle,
        attributes: *const c_void,
        protection: u32,
        maximum_size_high: u32,
        maximum_size_low: u32,
        name: *const u16,
    ) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn GetCurrentProcess() -> Handle;
    fn GetLastError() -> u32;
    fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
}

// Windows SDKs do not ship a KernelBase import library by default, so bind
// the placeholder APIs directly against kernelbase.dll.
#[link(name = "kernelbase", kind = "raw-dylib")]
unsafe extern "system" {
    fn VirtualAlloc2(
        process: Handle,
        base_address: *mut c_void,
        size: usize,
        allocation_type: u32,
        page_protection: u32,
        extended_parameters: *const c_void,
        parameter_count: u32,
    ) -> *mut c_void;
    fn MapViewOfFile3(
        file_mapping: Handle,
        process: Handle,
        base_address: *mut c_void,
        offset: u64,
        view_size: usize,
        allocation_type: u32,
        page_protection: u32,
        extended_parameters: *const c_void,
        parameter_count: u32,
    ) -> *mut c_void;
    fn UnmapViewOfFile2(process: Handle, base_address: *mut c_void, unmap_flags: u32) -> i32;
}

#[derive(Debug)]
struct SectionInner {
    handle: NonNull<c_void>,
    len: usize,
}

impl Drop for SectionInner {
    fn drop(&mut self) {
        // SAFETY: The constructor owns one valid mapping handle.
        let _ = unsafe { CloseHandle(self.handle.as_ptr()) };
    }
}

// SAFETY: Windows section handles can be used from multiple threads.
unsafe impl Send for SectionInner {}
// SAFETY: The wrapper does not expose mutable handle state.
unsafe impl Sync for SectionInner {}

/// A pagefile-backed physical memory section.
#[derive(Debug, Clone)]
pub struct PagefileSection {
    inner: Arc<SectionInner>,
}

impl PagefileSection {
    /// Creates a committed pagefile-backed section.
    pub fn new(len: usize) -> Result<Self, PlatformError> {
        if len == 0 {
            return Err(PlatformError::InvalidArgument("section length must not be zero"));
        }

        let size = len as u64;
        let high = (size >> 32) as u32;
        let low = size as u32;

        // SAFETY: The call uses the pagefile handle and null optional pointers.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                ptr::null(),
                PAGE_READWRITE,
                high,
                low,
                ptr::null(),
            )
        };
        let handle = NonNull::new(handle).ok_or_else(|| last_error("CreateFileMappingW"))?;

        Ok(Self { inner: Arc::new(SectionInner { handle, len }) })
    }

    /// Returns the section length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len
    }

    /// Returns true when the section has zero length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Replaces one complete placeholder with a section view.
    pub fn map_replace(
        &self,
        mut placeholder: Placeholder,
        offset: u64,
        protection: PageProtection,
    ) -> Result<MappedView, PlatformError> {
        let view_len = placeholder.len;
        let end = offset
            .checked_add(view_len as u64)
            .ok_or(PlatformError::InvalidArgument("section view range overflow"))?;
        if end > self.inner.len as u64 {
            return Err(PlatformError::InvalidArgument("section view exceeds the section"));
        }

        // SAFETY: The placeholder owns the exact address range passed to the call.
        let result = unsafe {
            MapViewOfFile3(
                self.inner.handle.as_ptr(),
                GetCurrentProcess(),
                placeholder.base.as_ptr(),
                offset,
                view_len,
                MEM_REPLACE_PLACEHOLDER,
                protection.to_raw(),
                ptr::null(),
                0,
            )
        };

        let base = NonNull::new(result).ok_or_else(|| last_error("MapViewOfFile3"))?;
        if base != placeholder.base {
            // SAFETY: The call removes the unexpected view at its returned base.
            let _ = unsafe { UnmapViewOfFile2(GetCurrentProcess(), base.as_ptr(), 0) };
            return Err(PlatformError::InvalidArgument(
                "MapViewOfFile3 returned a different placeholder address",
            ));
        }

        placeholder.armed = false;
        drop(placeholder);

        Ok(MappedView { base, len: view_len, section: self.clone() })
    }
}

/// A reserved Windows placeholder.
#[derive(Debug)]
pub struct Placeholder {
    base: NonNull<c_void>,
    len: usize,
    armed: bool,
}

impl Placeholder {
    /// Reserves one complete placeholder range.
    pub fn reserve(base: Option<usize>, len: usize) -> Result<Self, PlatformError> {
        if len == 0 {
            return Err(PlatformError::InvalidArgument("placeholder length must not be zero"));
        }

        let requested = base.map_or(ptr::null_mut(), |value| value as *mut c_void);

        // SAFETY: The optional address is caller-provided. Windows validates the range.
        let result = unsafe {
            VirtualAlloc2(
                GetCurrentProcess(),
                requested,
                len,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                ptr::null(),
                0,
            )
        };
        let result = NonNull::new(result).ok_or_else(|| last_error("VirtualAlloc2"))?;

        if let Some(requested) = base
            && result.as_ptr() as usize != requested
        {
            // SAFETY: The call releases the range returned by VirtualAlloc2.
            let _ = unsafe { VirtualFree(result.as_ptr(), 0, MEM_RELEASE) };
            return Err(PlatformError::InvalidArgument(
                "VirtualAlloc2 returned a different requested address",
            ));
        }

        Ok(Self { base: result, len, armed: true })
    }

    /// Returns the placeholder base.
    #[must_use]
    pub fn base(&self) -> usize {
        self.base.as_ptr() as usize
    }

    /// Returns the placeholder length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns true when the placeholder has zero length.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for Placeholder {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        // SAFETY: This object owns one reserved placeholder range.
        let _ = unsafe { VirtualFree(self.base.as_ptr(), 0, MEM_RELEASE) };
    }
}

/// A mapped section view that replaced one complete placeholder.
#[derive(Debug)]
pub struct MappedView {
    base: NonNull<c_void>,
    len: usize,
    section: PagefileSection,
}

impl MappedView {
    /// Returns the host base address.
    #[must_use]
    pub fn base(&self) -> usize {
        self.base.as_ptr() as usize
    }

    /// Returns the mapped length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns true when the view has zero length.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the backing section length.
    #[must_use]
    pub fn section_len(&self) -> usize {
        self.section.len()
    }
}

impl Drop for MappedView {
    fn drop(&mut self) {
        // SAFETY: This object owns the view at the exact base returned by MapViewOfFile3.
        let unmapped = unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), self.base.as_ptr(), MEM_PRESERVE_PLACEHOLDER)
        };
        if unmapped != 0 {
            // SAFETY: The successful unmap restored a placeholder for this complete range.
            let _ = unsafe { VirtualFree(self.base.as_ptr(), 0, MEM_RELEASE) };
            return;
        }

        // SAFETY: The fallback removes the owned view without preserving its placeholder.
        let _ = unsafe { UnmapViewOfFile2(GetCurrentProcess(), self.base.as_ptr(), 0) };
    }
}

impl PageProtection {
    const fn to_raw(self) -> u32 {
        match self {
            Self::NoAccess => PAGE_NOACCESS,
            Self::ReadOnly => PAGE_READONLY,
            Self::ReadWrite => PAGE_READWRITE,
            Self::ExecuteRead => PAGE_EXECUTE_READ,
            Self::ExecuteReadWrite => PAGE_EXECUTE_READWRITE,
        }
    }
}

fn last_error(operation: &'static str) -> PlatformError {
    // SAFETY: GetLastError takes no arguments and returns thread-local state.
    let code = unsafe { GetLastError() };
    PlatformError::Win32 { operation, code }
}
