//! Host code-buffer ownership with separate write and execute phases.
//!
//! A buffer is writable and non-executable while code lands in it. Sealing
//! consumes the writable owner, marks the pages execute-read, and flushes
//! the host instruction cache. No page is ever writable and executable at
//! the same time, and the sealed owner exposes no mutation.
//!
//! ```compile_fail
//! fn requires_clone<T: Clone>() {}
//! requires_clone::<exbawks_platform::WritableCodeBuffer>();
//! ```
//!
//! ```compile_fail
//! fn requires_clone<T: Clone>() {}
//! requires_clone::<exbawks_platform::ExecutableCodeBuffer>();
//! ```

use crate::PlatformError;

/// A writable, non-executable host code buffer.
#[derive(Debug)]
pub struct WritableCodeBuffer {
    imp: imp::CodeAllocation,
    len: usize,
}

impl WritableCodeBuffer {
    /// Allocates a writable code buffer with the requested capacity.
    pub fn new(capacity: usize) -> Result<Self, PlatformError> {
        if capacity == 0 {
            return Err(PlatformError::InvalidArgument("code buffer capacity must not be zero"));
        }

        Ok(Self { imp: imp::CodeAllocation::new(capacity)?, len: 0 })
    }

    /// Returns the allocated capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.imp.capacity()
    }

    /// Returns the written byte count.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns true when no bytes are written.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends bytes and returns their buffer offset.
    pub fn push(&mut self, bytes: &[u8]) -> Result<usize, PlatformError> {
        let offset = self.len;
        let end = offset
            .checked_add(bytes.len())
            .ok_or(PlatformError::InvalidArgument("code buffer write overflows"))?;
        if end > self.imp.capacity() {
            return Err(PlatformError::InvalidArgument("code buffer capacity is exhausted"));
        }

        self.imp.write_at(offset, bytes)?;
        self.len = end;
        Ok(offset)
    }

    /// Seals the buffer as execute-read memory and flushes the host
    /// instruction cache.
    pub fn seal(self) -> Result<ExecutableCodeBuffer, PlatformError> {
        let len = self.len;
        let imp = self.imp.seal()?;
        Ok(ExecutableCodeBuffer { imp, len })
    }
}

/// A sealed execute-read host code buffer.
#[derive(Debug)]
pub struct ExecutableCodeBuffer {
    imp: imp::CodeAllocation,
    len: usize,
}

impl ExecutableCodeBuffer {
    /// Returns the host base address.
    #[must_use]
    pub fn base(&self) -> usize {
        self.imp.base()
    }

    /// Returns the sealed byte count.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns true when no bytes were sealed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};

    use crate::PlatformError;

    const PAGE_READWRITE: u32 = 0x04;
    const PAGE_EXECUTE_READ: u32 = 0x20;
    const MEM_COMMIT: u32 = 0x0000_1000;
    const MEM_RESERVE: u32 = 0x0000_2000;
    const MEM_RELEASE: u32 = 0x0000_8000;

    unsafe extern "system" {
        fn VirtualAlloc(
            address: *mut c_void,
            size: usize,
            allocation_type: u32,
            protection: u32,
        ) -> *mut c_void;
        fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
        fn VirtualProtect(
            address: *mut c_void,
            size: usize,
            new_protection: u32,
            old_protection: *mut u32,
        ) -> i32;
        fn FlushInstructionCache(process: *mut c_void, address: *const c_void, size: usize) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn GetLastError() -> u32;
    }

    /// One committed private allocation for generated code.
    #[derive(Debug)]
    pub(super) struct CodeAllocation {
        base: NonNull<c_void>,
        capacity: usize,
    }

    // SAFETY: The owner controls one process-global allocation without
    // thread-affine state.
    unsafe impl Send for CodeAllocation {}
    // SAFETY: Shared methods only read the stored base and capacity.
    unsafe impl Sync for CodeAllocation {}

    impl CodeAllocation {
        pub(super) fn new(capacity: usize) -> Result<Self, PlatformError> {
            // SAFETY: The call reserves and commits a fresh private range
            // with no pointer preconditions.
            let base = unsafe {
                VirtualAlloc(ptr::null_mut(), capacity, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE)
            };
            let base = NonNull::new(base).ok_or_else(|| last_error("VirtualAlloc"))?;
            Ok(Self { base, capacity })
        }

        pub(super) const fn capacity(&self) -> usize {
            self.capacity
        }

        pub(super) fn base(&self) -> usize {
            self.base.as_ptr() as usize
        }

        pub(super) fn write_at(&self, offset: usize, bytes: &[u8]) -> Result<(), PlatformError> {
            let end = offset
                .checked_add(bytes.len())
                .ok_or(PlatformError::InvalidArgument("code buffer write overflows"))?;
            if end > self.capacity {
                return Err(PlatformError::InvalidArgument("code buffer write exceeds capacity"));
            }

            // SAFETY: This object keeps [base, base + capacity) committed and
            // writable until seal consumes it, the range is bounds-checked
            // above, and a safe caller cannot alias the private mapping.
            unsafe {
                ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    (self.base.as_ptr() as *mut u8).add(offset),
                    bytes.len(),
                );
            }
            Ok(())
        }

        pub(super) fn seal(self) -> Result<Self, PlatformError> {
            let mut old_protection = 0_u32;
            // SAFETY: This object owns the committed range, and the
            // out-pointer targets one writable stack value that outlives the
            // call.
            let protected = unsafe {
                VirtualProtect(
                    self.base.as_ptr(),
                    self.capacity,
                    PAGE_EXECUTE_READ,
                    &raw mut old_protection,
                )
            };
            if protected == 0 {
                return Err(last_error("VirtualProtect"));
            }

            // SAFETY: The range is owned, mapped, and newly execute-read.
            let flushed = unsafe {
                FlushInstructionCache(GetCurrentProcess(), self.base.as_ptr(), self.capacity)
            };
            if flushed == 0 {
                return Err(last_error("FlushInstructionCache"));
            }

            Ok(self)
        }
    }

    impl Drop for CodeAllocation {
        fn drop(&mut self) {
            // SAFETY: This object owns one committed private allocation.
            let _ = unsafe { VirtualFree(self.base.as_ptr(), 0, MEM_RELEASE) };
        }
    }

    fn last_error(operation: &'static str) -> PlatformError {
        // SAFETY: GetLastError takes no arguments and returns thread-local
        // state.
        let code = unsafe { GetLastError() };
        PlatformError::Win32 { operation, code }
    }
}

#[cfg(not(windows))]
mod imp {
    use crate::PlatformError;

    /// One committed private allocation for generated code.
    #[derive(Debug)]
    pub(super) struct CodeAllocation {
        capacity: usize,
    }

    impl CodeAllocation {
        pub(super) fn new(capacity: usize) -> Result<Self, PlatformError> {
            let _ = capacity;
            Err(PlatformError::Unsupported("code buffers require Windows"))
        }

        pub(super) const fn capacity(&self) -> usize {
            self.capacity
        }

        pub(super) const fn base(&self) -> usize {
            0
        }

        pub(super) fn write_at(&self, offset: usize, bytes: &[u8]) -> Result<(), PlatformError> {
            let _ = (offset, bytes);
            Err(PlatformError::Unsupported("code buffers require Windows"))
        }

        pub(super) fn seal(self) -> Result<Self, PlatformError> {
            Err(PlatformError::Unsupported("code buffers require Windows"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    mod windows {
        use std::ffi::c_void;

        use super::*;

        const PAGE_READWRITE: u32 = 0x04;
        const PAGE_EXECUTE_READ: u32 = 0x20;
        const MEM_FREE: u32 = 0x0001_0000;

        #[repr(C)]
        struct MemoryBasicInformation {
            base_address: *mut c_void,
            allocation_base: *mut c_void,
            allocation_protect: u32,
            partition_id: u16,
            region_size: usize,
            state: u32,
            protect: u32,
            kind: u32,
        }

        unsafe extern "system" {
            fn VirtualQuery(
                address: *const c_void,
                buffer: *mut MemoryBasicInformation,
                length: usize,
            ) -> usize;
        }

        fn query(address: usize) -> MemoryBasicInformation {
            let mut information = std::mem::MaybeUninit::<MemoryBasicInformation>::uninit();
            // SAFETY: The out-pointer targets one writable stack value of the
            // documented MEMORY_BASIC_INFORMATION layout that outlives the
            // call.
            let written = unsafe {
                VirtualQuery(
                    address as *const c_void,
                    information.as_mut_ptr(),
                    std::mem::size_of::<MemoryBasicInformation>(),
                )
            };
            assert_ne!(written, 0, "VirtualQuery must succeed");
            // SAFETY: A successful query initialized the complete value.
            unsafe { information.assume_init() }
        }

        #[test]
        fn sealed_buffer_executes_a_return_stub() {
            let mut buffer = WritableCodeBuffer::new(4096).expect("allocation succeeds");
            // mov rax, 0x2A; ret
            let offset = buffer
                .push(&[0x48, 0xC7, 0xC0, 0x2A, 0x00, 0x00, 0x00, 0xC3])
                .expect("write succeeds");
            let sealed = buffer.seal().expect("seal succeeds");

            let entry = sealed.base() + offset;
            // SAFETY: The sealed buffer maps execute-read code at the entry
            // address for the lifetime of `sealed`, and the stub follows the
            // host C ABI with no arguments.
            let stub: unsafe extern "C" fn() -> u64 = unsafe { std::mem::transmute(entry) };
            // SAFETY: The stub only sets RAX and returns.
            let value = unsafe { stub() };
            assert_eq!(value, 0x2A);
        }

        #[test]
        fn no_executable_page_remains_writable() {
            let mut buffer = WritableCodeBuffer::new(4096).expect("allocation succeeds");
            buffer.push(&[0xC3]).expect("write succeeds");
            assert_eq!(query(buffer.imp.base()).protect, PAGE_READWRITE);

            let sealed = buffer.seal().expect("seal succeeds");
            assert_eq!(query(sealed.base()).protect, PAGE_EXECUTE_READ);
        }

        #[test]
        fn drop_releases_the_allocation() {
            let buffer = WritableCodeBuffer::new(4096).expect("allocation succeeds");
            let base = buffer.imp.base();
            drop(buffer);
            assert_eq!(query(base).state, MEM_FREE);

            let sealed = WritableCodeBuffer::new(4096)
                .expect("allocation succeeds")
                .seal()
                .expect("seal succeeds");
            let base = sealed.base();
            drop(sealed);
            assert_eq!(query(base).state, MEM_FREE);
        }

        #[test]
        fn writes_are_bounds_checked() {
            let mut buffer = WritableCodeBuffer::new(4096).expect("allocation succeeds");
            let oversized = vec![0x90_u8; 4097];
            let error = buffer.push(&oversized).expect_err("oversized write must fail");
            assert!(matches!(error, PlatformError::InvalidArgument(_)));
            assert_eq!(buffer.len(), 0);

            buffer.push(&vec![0x90_u8; 4096]).expect("full write succeeds");
            let error = buffer.push(&[0x90]).expect_err("exhausted buffer must fail");
            assert!(matches!(error, PlatformError::InvalidArgument(_)));
            assert_eq!(buffer.len(), 4096);
        }
    }

    #[test]
    fn zero_capacity_is_rejected() {
        let error = WritableCodeBuffer::new(0).expect_err("zero capacity must fail");
        assert!(matches!(error, PlatformError::InvalidArgument(_)));
    }

    #[cfg(not(windows))]
    #[test]
    fn portable_hosts_report_unsupported() {
        let error = WritableCodeBuffer::new(4096).expect_err("allocation must fail");
        assert!(matches!(error, PlatformError::Unsupported(_)));
    }
}
