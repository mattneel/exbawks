#![forbid(unsafe_code)]
#![doc = "Guest CPU state and x86 block decoding for Exbawks."]

pub mod cache;
mod decode;
mod exec;
pub mod flags;
mod interpret;
mod sse;
mod state;
mod subset;
mod x87;

pub use decode::{
    BasicBlockDecoder, BlockDecodeError, BlockStop, DecodeConfig, DecodedBlock, format_instruction,
};
/// The decoded-instruction type callers hold between decode and step.
pub use iced_x86::Instruction as DecodedInstruction;

pub use exec::{
    ExecError, NoPorts, PortBus, decode_one, step, step_cached, step_instruction, step_with_ports,
};
pub use interpret::{InterpretError, step_register_only};
pub use state::{CpuState, Gpr, Segment, SegmentState, X87State};
pub use subset::{AluOp, RegisterOp, RegisterOperand, classify_register_op, indirect_call_slot};
