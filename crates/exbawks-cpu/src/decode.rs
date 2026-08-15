use exbawks_types::GuestVa;
use iced_x86::{Decoder, DecoderOptions, FlowControl, Formatter, Instruction, MasmFormatter};
use thiserror::Error;

/// Limits for one decoded guest basic block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeConfig {
    /// The maximum instruction count.
    pub max_instructions: usize,
    /// The maximum byte count.
    pub max_bytes: usize,
}

impl Default for DecodeConfig {
    fn default() -> Self {
        Self { max_instructions: 256, max_bytes: 4096 }
    }
}

/// The condition that ended block decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockStop {
    /// A control-flow instruction ended the block.
    ControlFlow(FlowControl),
    /// The byte slice ended.
    EndOfInput,
    /// The configured instruction limit ended decoding.
    InstructionLimit,
    /// The configured byte limit ended decoding.
    ByteLimit,
}

/// A decoded guest basic block.
#[derive(Debug, Clone)]
pub struct DecodedBlock {
    /// The first guest address.
    pub start: GuestVa,
    /// The decoded instructions.
    pub instructions: Vec<Instruction>,
    /// The number of consumed guest bytes.
    pub byte_len: usize,
    /// The reason decoding stopped.
    pub stop: BlockStop,
}

impl DecodedBlock {
    /// Returns the exclusive guest end address when it fits in 32 bits.
    #[must_use]
    pub fn end(&self) -> Option<GuestVa> {
        let length = u32::try_from(self.byte_len).ok()?;
        self.start.checked_add(length)
    }
}

/// An `iced-x86` basic-block decoder.
#[derive(Debug, Clone, Copy)]
pub struct BasicBlockDecoder {
    config: DecodeConfig,
}

impl BasicBlockDecoder {
    /// Creates a decoder with explicit limits.
    #[must_use]
    pub const fn new(config: DecodeConfig) -> Self {
        Self { config }
    }

    /// Decodes one guest block from the supplied executable bytes.
    pub fn decode(&self, start: GuestVa, bytes: &[u8]) -> Result<DecodedBlock, BlockDecodeError> {
        if self.config.max_instructions == 0 {
            return Err(BlockDecodeError::InvalidConfiguration(
                "max_instructions must not be zero",
            ));
        }
        if self.config.max_bytes == 0 {
            return Err(BlockDecodeError::InvalidConfiguration("max_bytes must not be zero"));
        }

        let window_len = bytes.len().min(self.config.max_bytes);
        let window = &bytes[..window_len];
        let mut decoder = Decoder::with_ip(32, window, u64::from(start.0), DecoderOptions::NONE);
        let mut instructions = Vec::new();
        let mut stop = BlockStop::EndOfInput;

        while decoder.can_decode() {
            if instructions.len() >= self.config.max_instructions {
                stop = BlockStop::InstructionLimit;
                break;
            }

            let instruction = decoder.decode();
            if instruction.is_invalid() {
                return Err(BlockDecodeError::InvalidInstruction {
                    address: GuestVa(u32::try_from(instruction.ip()).unwrap_or(u32::MAX)),
                });
            }

            let flow = instruction.flow_control();
            instructions.push(instruction);
            if flow != FlowControl::Next {
                stop = BlockStop::ControlFlow(flow);
                break;
            }
        }

        let byte_len = decoder.position();
        if matches!(stop, BlockStop::EndOfInput) && window_len < bytes.len() {
            stop = BlockStop::ByteLimit;
        }

        Ok(DecodedBlock { start, instructions, byte_len, stop })
    }
}

impl Default for BasicBlockDecoder {
    fn default() -> Self {
        Self::new(DecodeConfig::default())
    }
}

/// Formats one instruction with MASM syntax.
#[must_use]
pub fn format_instruction(instruction: &Instruction) -> String {
    let mut formatter = MasmFormatter::new();
    let mut output = String::new();
    formatter.format(instruction, &mut output);
    output
}

/// A guest block decode failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BlockDecodeError {
    /// Decoder limits were invalid.
    #[error("invalid block decoder configuration: {0}")]
    InvalidConfiguration(&'static str),
    /// iced-x86 returned an invalid instruction.
    #[error("invalid guest instruction at {address}")]
    InvalidInstruction { address: GuestVa },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_stops_after_return() {
        let decoder = BasicBlockDecoder::default();
        let block =
            decoder.decode(GuestVa(0x1000), &[0x90, 0x40, 0xC3, 0x90]).expect("block must decode");

        assert_eq!(block.instructions.len(), 3);
        assert_eq!(block.byte_len, 3);
        assert!(matches!(block.stop, BlockStop::ControlFlow(FlowControl::Return)));
    }

    #[test]
    fn byte_limit_is_reported() {
        let decoder = BasicBlockDecoder::new(DecodeConfig { max_instructions: 10, max_bytes: 2 });
        let block =
            decoder.decode(GuestVa(0x1000), &[0x90, 0x90, 0x90]).expect("block must decode");
        assert_eq!(block.stop, BlockStop::ByteLimit);
    }
}
