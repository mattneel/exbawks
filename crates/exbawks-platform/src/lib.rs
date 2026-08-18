#![doc = "Host platform services for Exbawks."]

mod aligned;
mod capabilities;
mod code_memory;
mod error;
#[cfg(windows)]
pub mod hid;
mod system_memory;
pub mod virtual_memory;
#[cfg(windows)]
pub mod window;

pub use aligned::AlignedBuffer;
pub use capabilities::{HostCapabilities, probe_host_capabilities};
pub use code_memory::{ExecutableCodeBuffer, WritableCodeBuffer};
pub use error::PlatformError;
pub use system_memory::{SystemMemoryInfo, query_system_memory_info};
