#![doc = "Windows Hypervisor Platform execution tier for Exbawks (ADR 0013)."]
#![doc = ""]
#![doc = "This crate runs the 32-bit guest on one WHP virtual processor. It is the"]
#![doc = "only crate besides `exbawks-platform` and `exbawks-jit` that holds unsafe"]
#![doc = "host FFI; every call documents its `SAFETY:` contract. On non-Windows"]
#![doc = "hosts every capability reports unavailable so the workspace still builds"]
#![doc = "for the portable-logic job, but the tier only functions on Windows x86-64."]

mod doctor;

pub use doctor::{WhpAvailability, probe_whp};
