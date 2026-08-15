use serde::{Deserialize, Serialize};

use crate::PlatformError;

/// Host memory geometry that constrains arena reservation and mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemMemoryInfo {
    /// The host page size in bytes.
    pub page_size: u32,
    /// The host allocation granularity in bytes.
    pub allocation_granularity: u32,
}

impl SystemMemoryInfo {
    /// Creates validated memory geometry.
    pub fn new(page_size: u32, allocation_granularity: u32) -> Result<Self, PlatformError> {
        if page_size == 0 || !page_size.is_power_of_two() {
            return Err(PlatformError::InvalidArgument(
                "host page size must be a nonzero power of two",
            ));
        }

        if allocation_granularity == 0 || !allocation_granularity.is_power_of_two() {
            return Err(PlatformError::InvalidArgument(
                "host allocation granularity must be a nonzero power of two",
            ));
        }

        if allocation_granularity < page_size {
            return Err(PlatformError::InvalidArgument(
                "host allocation granularity must not be smaller than the page size",
            ));
        }

        Ok(Self { page_size, allocation_granularity })
    }
}

/// Queries the host page size and allocation granularity.
pub fn query_system_memory_info() -> Result<SystemMemoryInfo, PlatformError> {
    imp::query_system_memory_info()
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::mem::MaybeUninit;

    use super::SystemMemoryInfo;
    use crate::PlatformError;

    #[repr(C)]
    struct SystemInfo {
        processor_architecture: u16,
        reserved: u16,
        page_size: u32,
        minimum_application_address: *mut c_void,
        maximum_application_address: *mut c_void,
        active_processor_mask: usize,
        number_of_processors: u32,
        processor_type: u32,
        allocation_granularity: u32,
        processor_level: u16,
        processor_revision: u16,
    }

    unsafe extern "system" {
        fn GetSystemInfo(system_info: *mut SystemInfo);
    }

    pub fn query_system_memory_info() -> Result<SystemMemoryInfo, PlatformError> {
        let mut info = MaybeUninit::<SystemInfo>::uninit();
        // SAFETY: The pointer targets one writable stack value that outlives the
        // call, matches the x86-64 SYSTEM_INFO layout and alignment, and the API
        // has no thread requirement.
        unsafe { GetSystemInfo(info.as_mut_ptr()) };
        // SAFETY: GetSystemInfo cannot fail and initialized the complete value.
        let info = unsafe { info.assume_init() };

        SystemMemoryInfo::new(info.page_size, info.allocation_granularity)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::SystemMemoryInfo;
    use crate::PlatformError;

    pub fn query_system_memory_info() -> Result<SystemMemoryInfo, PlatformError> {
        Err(PlatformError::Unsupported("system memory geometry requires Windows"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_accepts_common_windows_values() {
        let info = SystemMemoryInfo::new(4096, 65536).expect("geometry is valid");
        assert_eq!(info.page_size, 4096);
        assert_eq!(info.allocation_granularity, 65536);
    }

    #[test]
    fn geometry_rejects_zero_values() {
        assert!(SystemMemoryInfo::new(0, 65536).is_err());
        assert!(SystemMemoryInfo::new(4096, 0).is_err());
    }

    #[test]
    fn geometry_rejects_values_that_are_not_powers_of_two() {
        assert!(SystemMemoryInfo::new(4095, 65536).is_err());
        assert!(SystemMemoryInfo::new(4096, 65535).is_err());
    }

    #[test]
    fn geometry_rejects_granularity_below_the_page_size() {
        let error = SystemMemoryInfo::new(4096, 2048).expect_err("geometry must fail");
        assert!(matches!(error, PlatformError::InvalidArgument(_)));
    }

    #[cfg(windows)]
    #[test]
    fn windows_query_returns_valid_geometry() {
        let info = query_system_memory_info().expect("query succeeds on Windows");
        assert!(info.page_size.is_power_of_two());
        assert!(info.allocation_granularity.is_power_of_two());
        assert!(info.allocation_granularity >= info.page_size);
    }

    #[cfg(not(windows))]
    #[test]
    fn portable_query_reports_an_unsupported_host() {
        let error = query_system_memory_info().expect_err("query must fail");
        assert!(matches!(error, PlatformError::Unsupported(_)));
    }
}
