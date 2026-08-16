#![doc = "Windows Hypervisor Platform execution tier for Exbawks (ADR 0013)."]
#![doc = ""]
#![doc = "This crate runs the 32-bit guest on one WHP virtual processor. It is the"]
#![doc = "only crate besides `exbawks-platform` and `exbawks-jit` that holds unsafe"]
#![doc = "host FFI; every call documents its `SAFETY:` contract. On non-Windows"]
#![doc = "hosts every capability reports unavailable so the workspace still builds"]
#![doc = "for the portable-logic job, but the tier only functions on Windows x86-64."]

mod doctor;

pub use doctor::{WhpAvailability, probe_whp};

#[cfg(all(windows, target_arch = "x86_64"))]
mod api;
#[cfg(all(windows, target_arch = "x86_64"))]
mod machine;

#[cfg(all(windows, target_arch = "x86_64"))]
pub use api::WhpError;
#[cfg(all(windows, target_arch = "x86_64"))]
pub use machine::{
    Canceller, GuestException, HostRegion, Machine, MapFlags, MemoryAccess, Register,
    RegisterValue, VpExitContext, WhpExit,
};

/// Serializes hardware-touching tests across the workspace.
///
/// Concurrent partition bring-up, teardown, and large mappings are flaky on
/// real hypervisors (transient `HV_STATUS_INSUFFICIENT_MEMORY` under
/// parallel 64 MiB partitions), and the tier itself only ever runs one
/// machine. Every test that creates a [`Machine`] should hold this guard.
#[cfg(all(windows, target_arch = "x86_64"))]
#[doc(hidden)]
pub fn hardware_serial_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
