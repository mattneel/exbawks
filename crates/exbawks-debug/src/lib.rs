#![forbid(unsafe_code)]
#![doc = "Debugger primitives and structured trace events for Exbawks."]

mod breakpoint;
mod trace;

pub use breakpoint::BreakpointSet;
pub use trace::{NoopTrace, TraceEvent, TraceSink, VecTrace};
