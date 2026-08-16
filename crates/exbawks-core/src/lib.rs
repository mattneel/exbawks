#![forbid(unsafe_code)]
#![doc = "Emulator composition and boot planning for Exbawks."]

mod config;
mod emulator;
mod error;
mod hostfs;
mod loaded;
mod report;
mod threads;
mod thunk;

pub use config::EmulatorConfig;
pub use emulator::{Emulator, EmulatorBuilder, EntryBlockPlan, GateAssist};
pub use error::CoreError;
pub use loaded::LoadedImage;
pub use report::{BootPlanReport, TranslationActionReport};
pub use thunk::{KernelThunk, KernelThunkTable};
