#![doc = "Host platform services for Exbawks."]

mod capabilities;
mod error;
pub mod virtual_memory;

pub use capabilities::{HostCapabilities, probe_host_capabilities};
pub use error::PlatformError;
