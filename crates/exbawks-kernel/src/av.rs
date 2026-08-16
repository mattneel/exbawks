//! Av* video-encoder exports.
//!
//! Direct3D's display bring-up talks to the TV encoder through these:
//! option queries pick an output standard, and `AvSetDisplayMode` programs
//! the final mode. There is no real encoder; queries answer a synthetic
//! NTSC composite profile and mode-set succeeds, which is exactly what the
//! title needs to proceed to its swap chain.

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
use crate::{KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus};

/// Registers the Av* exports.
pub(crate) fn register_av_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(AvGetSavedDataAddress)?;
    registry.register(AvSendTVEncoderOption)?;
    registry.register(AvSetDisplayMode)?;
    registry.register(AvSetSavedDataAddress)?;
    Ok(())
}

/// Returns the persisted-frame buffer address (none: NULL).
#[derive(Debug, Default, Clone, Copy)]
pub struct AvGetSavedDataAddress;

impl KernelExport for AvGetSavedDataAddress {
    fn ordinal(&self) -> u16 {
        crate::ordinal::AV_GET_SAVED_DATA_ADDRESS
    }

    fn name(&self) -> &'static str {
        "AvGetSavedDataAddress"
    }

    fn stack_bytes(&self) -> u16 {
        0
    }

    fn call(&self, _context: &mut KernelCallContext<'_>) -> KernelStatus {
        // No frame survives a reboot here; NULL tells the caller to start
        // fresh.
        KernelStatus(0)
    }
}

/// Answers one TV-encoder option query or setting.
#[derive(Debug, Default, Clone, Copy)]
pub struct AvSendTVEncoderOption;

impl KernelExport for AvSendTVEncoderOption {
    fn ordinal(&self) -> u16 {
        crate::ordinal::AV_SEND_TV_ENCODER_OPTION
    }

    fn name(&self) -> &'static str {
        "AvSendTVEncoderOption"
    }

    fn stack_bytes(&self) -> u16 {
        16
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // AvSendTVEncoderOption(RegisterBase, Option, Param, Result).
        let option = stack_argument(context, 1).unwrap_or(0);
        let parameter = stack_argument(context, 2).unwrap_or(0);
        let result_out = stack_argument(context, 3).unwrap_or(0);
        tracing::debug!(option, parameter, "AvSendTVEncoderOption");

        // Queries answer a synthetic profile; settings are accepted.
        // Option 4 (`AV_QUERY_MODE_TABLE_VERSION`-family) and the flicker/
        // cable queries all take zero safely; the one that matters is the
        // display-standard query, answered as NTSC-M composite.
        const AV_OPTION_QUERY_MODE: u32 = 6;
        let value = match option {
            AV_OPTION_QUERY_MODE => 0x0000_0001, // NTSC-M
            _ => 0,
        };
        if result_out != 0 {
            let _ = context.memory.write_u32(GuestVa(result_out), value);
        }
        KernelStatus::SUCCESS
    }
}

/// Programs the display mode; the frame's address and format are recorded
/// by the graphics model once one exists.
#[derive(Debug, Default, Clone, Copy)]
pub struct AvSetDisplayMode;

impl KernelExport for AvSetDisplayMode {
    fn ordinal(&self) -> u16 {
        crate::ordinal::AV_SET_DISPLAY_MODE
    }

    fn name(&self) -> &'static str {
        "AvSetDisplayMode"
    }

    fn stack_bytes(&self) -> u16 {
        24
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // AvSetDisplayMode(RegisterBase, Step, Mode, Format, Pitch,
        //                  FrameBuffer).
        let mode = stack_argument(context, 2).unwrap_or(0);
        let format = stack_argument(context, 3).unwrap_or(0);
        let pitch = stack_argument(context, 4).unwrap_or(0);
        let frame_buffer = stack_argument(context, 5).unwrap_or(0);
        tracing::info!(
            mode = format_args!("{mode:#x}"),
            format = format_args!("{format:#x}"),
            pitch,
            frame_buffer = format_args!("{frame_buffer:#010x}"),
            "display mode set"
        );
        KernelStatus::SUCCESS
    }
}

/// Records the address a frame should persist at across reboots.
#[derive(Debug, Default, Clone, Copy)]
pub struct AvSetSavedDataAddress;

impl KernelExport for AvSetSavedDataAddress {
    fn ordinal(&self) -> u16 {
        crate::ordinal::AV_SET_SAVED_DATA_ADDRESS
    }

    fn name(&self) -> &'static str {
        "AvSetSavedDataAddress"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, _context: &mut KernelCallContext<'_>) -> KernelStatus {
        KernelStatus::SUCCESS
    }
}
