#![doc = "Host platform services for Exbawks."]

mod capabilities;
mod code_memory;
mod error;
mod system_memory;
pub mod virtual_memory;

pub use capabilities::{HostCapabilities, probe_host_capabilities};
pub use code_memory::{ExecutableCodeBuffer, WritableCodeBuffer};
pub use error::PlatformError;
pub use system_memory::{SystemMemoryInfo, query_system_memory_info};
