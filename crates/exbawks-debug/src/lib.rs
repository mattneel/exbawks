#![forbid(unsafe_code)]
#![doc = "Debugger primitives and structured trace events for Exbawks."]

mod breakpoint;
mod json_trace;
mod source_map;
mod trace;

pub use breakpoint::BreakpointSet;
pub use json_trace::{JsonLinesTrace, TraceRecord};
pub use source_map::{BlockSourceMap, FaultSite, SourceMapError, SourceRange};
pub use trace::{NoopTrace, TraceEvent, TraceEventKind, TraceSink, VecTrace};
