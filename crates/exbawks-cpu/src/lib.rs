#![forbid(unsafe_code)]
#![doc = "Guest CPU state and x86 block decoding for Exbawks."]

mod decode;
mod state;

pub use decode::{
    BasicBlockDecoder, BlockDecodeError, BlockStop, DecodeConfig, DecodedBlock,
    format_instruction,
};
pub use state::{CpuState, Gpr, Segment, SegmentState, X87State};
