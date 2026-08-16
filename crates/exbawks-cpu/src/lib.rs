#![forbid(unsafe_code)]
#![doc = "Guest CPU state and x86 block decoding for Exbawks."]

mod decode;
mod exec;
pub mod flags;
mod interpret;
mod state;
mod subset;

pub use decode::{
    BasicBlockDecoder, BlockDecodeError, BlockStop, DecodeConfig, DecodedBlock, format_instruction,
};
pub use exec::{ExecError, step};
pub use interpret::{InterpretError, step_register_only};
pub use state::{CpuState, Gpr, Segment, SegmentState, X87State};
pub use subset::{AluOp, RegisterOp, RegisterOperand, classify_register_op, indirect_call_slot};
