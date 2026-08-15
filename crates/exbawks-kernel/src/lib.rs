#![forbid(unsafe_code)]
#![doc = "Kernel high-level emulation interfaces for Exbawks."]

mod context;
mod error;
mod export;
mod gate;
mod registry;
mod status;

pub use context::KernelCallContext;
pub use error::KernelError;
pub use export::{KernelExport, StubExport};
pub use gate::{KERNEL_GATE_BASE, KERNEL_GATE_END, gate_address, gate_ordinal};
pub use registry::KernelRegistry;
pub use status::KernelStatus;
