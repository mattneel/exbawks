#![forbid(unsafe_code)]
#![doc = "Kernel high-level emulation interfaces for Exbawks."]

mod context;
mod error;
mod ex;
mod export;
mod gate;
mod ke;
mod ordinals;
mod registry;
mod rtl;
mod services;
mod startup;
mod status;

pub use context::KernelCallContext;
pub use error::KernelError;
pub use ex::ExQueryNonVolatileSetting;
pub use export::{KernelExport, StubExport, SuccessExport};
pub use gate::{KERNEL_GATE_BASE, KERNEL_GATE_END, gate_address, gate_ordinal};
pub use ke::KeInitializeDpc;
pub use ordinals::{
    CallingConvention, ExportKind, KERNEL_ORDINALS, KernelOrdinalInfo, kernel_ordinal_info,
};
pub use registry::KernelRegistry;
pub use rtl::{
    RtlEnterCriticalSection, RtlInitializeCriticalSection, RtlLeaveCriticalSection,
    RtlNtStatusToDosError,
};
pub use services::{
    KernelServiceError, KernelServices, ThreadCreateRequest, ThreadCreated, UnsupportedServices,
};
pub use startup::{
    DbgPrint, HalReturnToFirmware, NtClose, PsCreateSystemThreadEx, PsTerminateSystemThread,
    ordinal, register_startup_exports,
};
pub use status::KernelStatus;
