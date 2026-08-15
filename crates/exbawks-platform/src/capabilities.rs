use serde::{Deserialize, Serialize};

/// Host capabilities that affect Exbawks runtime modes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCapabilities {
    /// The compile-time host operating system.
    pub operating_system: String,
    /// The compile-time host architecture.
    pub architecture: String,
    /// True for the supported Windows x86-64 runtime target.
    pub supported_runtime_target: bool,
    /// True when the CPU and OS expose FSGSBASE instructions.
    pub fsgsbase: bool,
    /// True when the target can use Windows placeholder APIs.
    pub placeholder_views: bool,
}

/// Probes capabilities that the runtime can select dynamically.
#[must_use]
pub fn probe_host_capabilities() -> HostCapabilities {
    HostCapabilities {
        operating_system: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        supported_runtime_target: cfg!(all(windows, target_arch = "x86_64")),
        fsgsbase: probe_fsgsbase(),
        placeholder_views: cfg!(windows),
    }
}

#[cfg(windows)]
fn probe_fsgsbase() -> bool {
    const PF_RDWRFSGSBASE_AVAILABLE: u32 = 22;

    unsafe extern "system" {
        fn IsProcessorFeaturePresent(processor_feature: u32) -> i32;
    }

    // SAFETY: The function takes one value parameter and has no pointer requirements.
    unsafe { IsProcessorFeaturePresent(PF_RDWRFSGSBASE_AVAILABLE) != 0 }
}

#[cfg(not(windows))]
const fn probe_fsgsbase() -> bool {
    false
}
