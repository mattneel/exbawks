#![forbid(unsafe_code)]
#![doc = "Kernel high-level emulation interfaces for Exbawks."]

mod context;
mod error;
mod export;
mod gate;
mod ordinals;
mod registry;
mod services;
mod startup;
mod status;

pub use context::KernelCallContext;
pub use error::KernelError;
pub use export::{KernelExport, StubExport};
pub use gate::{KERNEL_GATE_BASE, KERNEL_GATE_END, gate_address, gate_ordinal};
pub use ordinals::{
    CallingConvention, ExportKind, KERNEL_ORDINALS, KernelOrdinalInfo, kernel_ordinal_info,
};
pub use registry::KernelRegistry;
pub use services::{
    KernelServiceError, KernelServices, ThreadCreateRequest, ThreadCreated, UnsupportedServices,
};
pub use startup::{
    DbgPrint, HalReturnToFirmware, PsCreateSystemThreadEx, PsTerminateSystemThread, ordinal,
    register_startup_exports,
};
pub use status::KernelStatus;
