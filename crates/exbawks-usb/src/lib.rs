#![forbid(unsafe_code)]
#![doc = "USB host controller and input device model for Exbawks."]

pub mod gamepad;
pub mod ohci;

pub use gamepad::{GamepadState, InputSource, NoInput, ScriptedInput, button};
pub use ohci::{OhciController, UsbMemory};
