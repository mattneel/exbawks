#![forbid(unsafe_code)]
#![doc = "Kernel high-level emulation interfaces for Exbawks."]

mod av;
mod context;
mod dispatcher;
mod error;
mod ex;
mod export;
mod file;
mod gate;
mod io;
mod irql;
mod ke;
mod mm;
mod mutant;
mod ordinals;
mod registry;
mod rtl;
mod services;
mod startup;
mod status;
mod vm;
mod xe;

pub use av::{
    AvGetSavedDataAddress, AvSendTVEncoderOption, AvSetDisplayMode, AvSetSavedDataAddress,
};
pub use context::KernelCallContext;
pub use dispatcher::{KeSetEvent, KeWaitForSingleObject};
pub use error::KernelError;
pub use ex::{
    ExAllocatePool, ExAllocatePoolWithTag, ExFreePool, ExQueryNonVolatileSetting,
    ExQueryPoolBlockSize,
};
pub use export::{KernelExport, StubExport, SuccessExport};
pub use file::{
    NtCreateFile, NtOpenFile, NtQueryInformationFile, NtReadFile, NtSetInformationFile, NtWriteFile,
};
pub use gate::{KERNEL_GATE_BASE, KERNEL_GATE_END, gate_address, gate_ordinal};
pub use io::{
    IoCreateSymbolicLink, IoDeleteSymbolicLink, NtOpenSymbolicLinkObject, NtQuerySymbolicLinkObject,
};
pub use irql::{
    HalGetInterruptVector, KeConnectInterrupt, KeGetCurrentIrql, KeInitializeInterrupt,
    KfLowerIrql, KfRaiseIrql,
};
pub use ke::{KeInitializeDpc, KeQuerySystemTime};
pub use mm::{MmAllocateContiguousMemory, MmGetPhysicalAddress};
pub use mutant::{NtCreateMutant, NtReleaseMutant};
pub use ordinals::{
    CallingConvention, ExportKind, KERNEL_ORDINALS, KernelOrdinalInfo, kernel_ordinal_info,
};
pub use registry::KernelRegistry;
pub use rtl::{
    RtlEnterCriticalSection, RtlEqualString, RtlInitAnsiString, RtlInitializeCriticalSection,
    RtlLeaveCriticalSection, RtlNtStatusToDosError,
};
pub use services::{
    DisplayMode, FileInfo, FileOpenRequest, FileOpened, KernelServiceError, KernelServices,
    ThreadCreateRequest, ThreadCreated, UnsupportedServices, VirtualAllocRequest,
    VirtualAllocation, WaitOutcome,
};
pub use startup::{
    DbgPrint, HalReturnToFirmware, NtClose, NtCreateEvent, NtResumeThread, NtSetEvent,
    NtSuspendThread, NtWaitForMultipleObjectsEx, NtWaitForSingleObject, NtWaitForSingleObjectEx,
    PsCreateSystemThreadEx, PsTerminateSystemThread, ordinal, register_startup_exports,
};
pub use status::KernelStatus;
pub use vm::NtAllocateVirtualMemory;
pub use xe::{XeLoadSection, XeUnloadSection};
