#![forbid(unsafe_code)]
#![doc = "USB host controller and input device model for Exbawks."]

pub mod device;
pub mod dualsense;
pub mod gamepad;
pub mod ohci;

pub use device::{GamepadDevice, Setup};
pub use dualsense::translate as translate_controller_report;
pub use gamepad::{GamepadState, InputSource, NoInput, ScriptedInput, button};
pub use ohci::{OhciController, UsbMemory};
