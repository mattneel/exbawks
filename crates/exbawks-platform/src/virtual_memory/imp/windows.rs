use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use crate::PlatformError;

use super::super::{CoalesceError, PageProtection, ReplaceError, RestoreError};

const PAGE_NOACCESS: u32 = 0x01;
const PAGE_READONLY: u32 = 0x02;
const PAGE_READWRITE: u32 = 0x04;
const PAGE_EXECUTE_READ: u32 = 0x20;
const PAGE_EXECUTE_READWRITE: u32 = 0x40;

const MEM_RESERVE: u32 = 0x0000_2000;
const MEM_REPLACE_PLACEHOLDER: u32 = 0x0000_4000;
const MEM_RESERVE_PLACEHOLDER: u32 = 0x0004_0000;
const MEM_RELEASE: u32 = 0x0000_8000;
const MEM_COALESCE_PLACEHOLDERS: u32 = 0x0000_0001;
const MEM_PRESERVE_PLACEHOLDER: u32 = 0x0000_0002;

const INVALID_HANDLE_VALUE: *mut c_void = -1_isize as *mut c_void;

const MEM_EXTENDED_PARAMETER_ADDRESS_REQUIREMENTS: u64 = 1;

#[repr(C)]
struct MemAddressRequirements {
    lowest_starting_address: *mut c_void,
    highest_ending_address: *mut c_void,
    alignment: usize,
}

#[repr(C)]
struct MemExtendedParameter {
    parameter_type: u64,
    pointer: *const c_void,
}

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
    ///
    /// A failure returns the preserved placeholder to the caller.
    pub fn map_replace(
        &self,
        mut placeholder: Placeholder,
        offset: u64,
        protection: PageProtection,
    ) -> Result<MappedView, ReplaceError> {
        let view_len = placeholder.len;
        let end = match offset.checked_add(view_len as u64) {
            Some(end) => end,
            None => {
                return Err(ReplaceError {
                    placeholder,
                    error: PlatformError::InvalidArgument("section view range overflow"),
                });
            }
        };
        if end > self.inner.len as u64 {
            return Err(ReplaceError {
                placeholder,
                error: PlatformError::InvalidArgument("section view exceeds the section"),
            });
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

        let Some(base) = NonNull::new(result) else {
            return Err(ReplaceError { placeholder, error: last_error("MapViewOfFile3") });
        };
        if base != placeholder.base {
            // SAFETY: The call removes the unexpected view at its returned base.
            let _ = unsafe { UnmapViewOfFile2(GetCurrentProcess(), base.as_ptr(), 0) };
            return Err(ReplaceError {
                placeholder,
                error: PlatformError::InvalidArgument(
                    "MapViewOfFile3 returned a different placeholder address",
                ),
            });
        }

        placeholder.armed = false;
        drop(placeholder);

        Ok(MappedView { base, len: view_len, protection, section: self.clone() })
    }
}

/// A reserved Windows placeholder.
#[derive(Debug)]
pub struct Placeholder {
    base: NonNull<c_void>,
    len: usize,
    armed: bool,
}

// SAFETY: The owner controls one process-global reservation without
// thread-affine state.
unsafe impl Send for Placeholder {}
// SAFETY: Shared methods only read the stored base and length.
unsafe impl Sync for Placeholder {}

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

    /// Reserves one aligned placeholder at a host-selected address.
    pub fn reserve_aligned(alignment: usize, len: usize) -> Result<Self, PlatformError> {
        if len == 0 {
            return Err(PlatformError::InvalidArgument("placeholder length must not be zero"));
        }

        let granularity = crate::query_system_memory_info()?.allocation_granularity as usize;
        if !alignment.is_power_of_two() || alignment < granularity {
            return Err(PlatformError::InvalidArgument(
                "placeholder alignment must be a power of two at least the allocation granularity",
            ));
        }

        let requirements = MemAddressRequirements {
            lowest_starting_address: ptr::null_mut(),
            highest_ending_address: ptr::null_mut(),
            alignment,
        };
        let parameter = MemExtendedParameter {
            parameter_type: MEM_EXTENDED_PARAMETER_ADDRESS_REQUIREMENTS,
            pointer: (&raw const requirements).cast(),
        };

        // SAFETY: The extended parameter points at one address-requirements
        // value that outlives the call on this stack frame.
        let result = unsafe {
            VirtualAlloc2(
                GetCurrentProcess(),
                ptr::null_mut(),
                len,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                (&raw const parameter).cast(),
                1,
            )
        };
        let result = NonNull::new(result).ok_or_else(|| last_error("VirtualAlloc2"))?;

        if !(result.as_ptr() as usize).is_multiple_of(alignment) {
            // SAFETY: The call releases the range returned by VirtualAlloc2.
            let _ = unsafe { VirtualFree(result.as_ptr(), 0, MEM_RELEASE) };
            return Err(PlatformError::InvalidArgument(
                "VirtualAlloc2 returned an unaligned address",
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

    /// Splits this placeholder and returns the owned tail range.
    ///
    /// A failed split leaves this placeholder unchanged.
    pub fn split_off(&mut self, offset: usize) -> Result<Self, PlatformError> {
        let page_size = crate::query_system_memory_info()?.page_size as usize;
        if offset == 0 || offset >= self.len {
            return Err(PlatformError::InvalidArgument(
                "split offset must fall inside the placeholder",
            ));
        }
        if !offset.is_multiple_of(page_size) || !(self.len - offset).is_multiple_of(page_size) {
            return Err(PlatformError::InvalidArgument("split offset must be page aligned"));
        }

        let tail_len = self.len - offset;
        let tail_base = self.base.as_ptr() as usize + offset;

        // SAFETY: This object owns the placeholder that contains the tail
        // range, and the flags split it without releasing the reservation.
        let split = unsafe {
            VirtualFree(tail_base as *mut c_void, tail_len, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER)
        };
        if split == 0 {
            return Err(last_error("VirtualFree(MEM_PRESERVE_PLACEHOLDER)"));
        }

        let tail_base = NonNull::new(tail_base as *mut c_void)
            .expect("a nonzero base plus a positive offset cannot be null");
        self.len = offset;
        Ok(Self { base: tail_base, len: tail_len, armed: true })
    }

    /// Coalesces this placeholder with the adjacent following placeholder.
    ///
    /// A failure returns the unconsumed placeholder to the caller.
    pub fn coalesce_with(&mut self, next: Self) -> Result<(), CoalesceError> {
        let expected_base = self.base.as_ptr() as usize + self.len;
        if next.base.as_ptr() as usize != expected_base {
            return Err(CoalesceError {
                next,
                error: PlatformError::InvalidArgument(
                    "coalesce requires an adjacent following placeholder",
                ),
            });
        }

        let Some(total_len) = self.len.checked_add(next.len) else {
            return Err(CoalesceError {
                next,
                error: PlatformError::InvalidArgument("coalesced placeholder length overflows"),
            });
        };

        // SAFETY: The two objects own the complete adjacent range, and the
        // flags merge the placeholders without releasing the reservation.
        let merged = unsafe {
            VirtualFree(self.base.as_ptr(), total_len, MEM_RELEASE | MEM_COALESCE_PLACEHOLDERS)
        };
        if merged == 0 {
            return Err(CoalesceError {
                next,
                error: last_error("VirtualFree(MEM_COALESCE_PLACEHOLDERS)"),
            });
        }

        let mut next = next;
        next.armed = false;
        self.len = total_len;
        Ok(())
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
    protection: PageProtection,
    section: PagefileSection,
}

// SAFETY: The owner controls one process-global mapping without
// thread-affine state.
unsafe impl Send for MappedView {}
// SAFETY: Shared copies target disjoint or caller-serialized ranges of one
// process-global mapping.
unsafe impl Sync for MappedView {}

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

    /// Returns the current host protection.
    #[must_use]
    pub const fn protection(&self) -> PageProtection {
        self.protection
    }

    /// Copies bytes out of the mapped view.
    pub fn read_at(&self, offset: usize, output: &mut [u8]) -> Result<(), PlatformError> {
        let end = offset
            .checked_add(output.len())
            .ok_or(PlatformError::InvalidArgument("view read range overflow"))?;
        if end > self.len {
            return Err(PlatformError::InvalidArgument("view read exceeds the mapped range"));
        }
        if matches!(self.protection, PageProtection::NoAccess) {
            return Err(PlatformError::InvalidArgument("view protection denies reads"));
        }

        // SAFETY: This object keeps [base, base + len) mapped with readable
        // protection for its complete lifetime, the range is bounds-checked
        // above, and a safe caller cannot alias the raw mapping with `output`.
        unsafe {
            ptr::copy_nonoverlapping(
                (self.base.as_ptr() as *const u8).add(offset),
                output.as_mut_ptr(),
                output.len(),
            );
        }
        Ok(())
    }

    /// Copies bytes into the mapped view.
    pub fn write_at(&self, offset: usize, input: &[u8]) -> Result<(), PlatformError> {
        let end = offset
            .checked_add(input.len())
            .ok_or(PlatformError::InvalidArgument("view write range overflow"))?;
        if end > self.len {
            return Err(PlatformError::InvalidArgument("view write exceeds the mapped range"));
        }
        if !matches!(self.protection, PageProtection::ReadWrite | PageProtection::ExecuteReadWrite)
        {
            return Err(PlatformError::InvalidArgument("view protection denies writes"));
        }

        // SAFETY: This object keeps [base, base + len) mapped with writable
        // protection for its complete lifetime, the range is bounds-checked
        // above, and a safe caller cannot alias the raw mapping with `input`.
        unsafe {
            ptr::copy_nonoverlapping(
                input.as_ptr(),
                (self.base.as_ptr() as *mut u8).add(offset),
                input.len(),
            );
        }
        Ok(())
    }

    /// Unmaps the view and returns the restored placeholder.
    ///
    /// A failure returns the still-mapped view to the caller.
    pub fn unmap_restore(self) -> Result<Placeholder, RestoreError> {
        // SAFETY: This object owns the view at the exact base returned by
        // MapViewOfFile3, and the flag restores its placeholder.
        let unmapped = unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), self.base.as_ptr(), MEM_PRESERVE_PLACEHOLDER)
        };
        if unmapped == 0 {
            let error = last_error("UnmapViewOfFile2(MEM_PRESERVE_PLACEHOLDER)");
            return Err(RestoreError { view: self, error });
        }

        let base = self.base;
        let len = self.len;
        let this = ManuallyDrop::new(self);
        // SAFETY: `this` skips its normal drop, so the section field is moved
        // out exactly once and no other field needs a destructor.
        drop(unsafe { ptr::read(&this.section) });

        Ok(Placeholder { base, len, armed: true })
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

#[cfg(test)]
mod tests {
    use super::*;

    const GRANULARITY: usize = 64 * 1024;
    const PAGE: usize = 4096;

    #[test]
    fn split_produces_three_owned_ranges() {
        let mut first = Placeholder::reserve(None, 3 * GRANULARITY).expect("reserve succeeds");
        let base = first.base();
        let mut second = first.split_off(GRANULARITY).expect("first split succeeds");
        let third = second.split_off(GRANULARITY).expect("second split succeeds");

        assert_eq!(first.base(), base);
        assert_eq!(first.len(), GRANULARITY);
        assert_eq!(second.base(), base + GRANULARITY);
        assert_eq!(second.len(), GRANULARITY);
        assert_eq!(third.base(), base + 2 * GRANULARITY);
        assert_eq!(third.len(), GRANULARITY);

        // Replacing the middle range proves that each range is one
        // independent placeholder.
        let section = PagefileSection::new(GRANULARITY).expect("section succeeds");
        let view =
            section.map_replace(second, 0, PageProtection::ReadWrite).expect("view succeeds");
        assert_eq!(view.base(), base + GRANULARITY);
    }

    #[test]
    fn coalesce_restores_one_placeholder() {
        let mut first = Placeholder::reserve(None, 3 * GRANULARITY).expect("reserve succeeds");
        let base = first.base();
        let mut second = first.split_off(GRANULARITY).expect("first split succeeds");
        let third = second.split_off(GRANULARITY).expect("second split succeeds");

        second.coalesce_with(third).expect("tail coalesce succeeds");
        first.coalesce_with(second).expect("head coalesce succeeds");
        assert_eq!(first.base(), base);
        assert_eq!(first.len(), 3 * GRANULARITY);

        // One full-range view replacement proves that one placeholder remains.
        let section = PagefileSection::new(3 * GRANULARITY).expect("section succeeds");
        let view = section.map_replace(first, 0, PageProtection::ReadOnly).expect("view succeeds");
        assert_eq!(view.len(), 3 * GRANULARITY);
    }

    #[test]
    fn failed_split_preserves_valid_ownership() {
        let mut placeholder =
            Placeholder::reserve(None, 2 * GRANULARITY).expect("reserve succeeds");

        let zero = placeholder.split_off(0).expect_err("zero offset must fail");
        assert!(matches!(zero, PlatformError::InvalidArgument(_)));
        let outside = placeholder.split_off(2 * GRANULARITY).expect_err("end offset must fail");
        assert!(matches!(outside, PlatformError::InvalidArgument(_)));
        let unaligned =
            placeholder.split_off(GRANULARITY + 1).expect_err("unaligned offset must fail");
        assert!(matches!(unaligned, PlatformError::InvalidArgument(_)));

        assert_eq!(placeholder.len(), 2 * GRANULARITY);
        let tail = placeholder.split_off(GRANULARITY).expect("valid split still succeeds");
        assert_eq!(placeholder.len(), GRANULARITY);
        assert_eq!(tail.len(), GRANULARITY);
    }

    #[test]
    fn failed_coalesce_returns_the_next_placeholder() {
        let mut first = Placeholder::reserve(None, 3 * GRANULARITY).expect("reserve succeeds");
        let mut second = first.split_off(GRANULARITY).expect("first split succeeds");
        let third = second.split_off(GRANULARITY).expect("second split succeeds");

        let error = first.coalesce_with(third).expect_err("skipping a range must fail");
        assert!(matches!(error.error, PlatformError::InvalidArgument(_)));

        let third = error.next;
        assert_eq!(third.len(), GRANULARITY);
        second.coalesce_with(third).expect("adjacent coalesce still succeeds");
        assert_eq!(second.len(), 2 * GRANULARITY);
    }

    #[test]
    fn split_supports_page_granularity() {
        let mut placeholder = Placeholder::reserve(None, GRANULARITY).expect("reserve succeeds");
        let tail = placeholder.split_off(PAGE).expect("page-granular split succeeds");
        assert_eq!(placeholder.len(), PAGE);
        assert_eq!(tail.len(), GRANULARITY - PAGE);
    }

    #[test]
    fn page_granular_view_replacement_works() {
        let mut placeholder = Placeholder::reserve(None, GRANULARITY).expect("reserve succeeds");
        let tail = placeholder.split_off(PAGE).expect("page-granular split succeeds");

        let section = PagefileSection::new(GRANULARITY).expect("section succeeds");
        let head_view = section
            .map_replace(placeholder, 0, PageProtection::ReadWrite)
            .expect("page-sized view succeeds");
        let tail_view = section
            .map_replace(tail, PAGE as u64, PageProtection::ReadWrite)
            .expect("page-offset view succeeds");
        assert_eq!(head_view.len(), PAGE);
        assert_eq!(tail_view.len(), GRANULARITY - PAGE);
    }
}
