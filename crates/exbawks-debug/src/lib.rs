#![forbid(unsafe_code)]
#![doc = "Debugger primitives and structured trace events for Exbawks."]

mod breakpoint;
mod coverage;
mod diagnose;
mod golden;
mod json_trace;
mod png;
mod source_map;
mod trace;

pub use breakpoint::BreakpointSet;
pub use coverage::{
    CoverageGap, CoverageItem, CoverageLedger, CoverageStatus, Surface, SurfaceCoverage,
};
pub use diagnose::render_site;
pub use golden::{FrameDifference, compare_frames, frame_digest};
pub use json_trace::{JsonLinesTrace, TraceRecord};
pub use png::encode_rgba;
pub use source_map::{BlockSourceMap, FaultSite, SourceMapError, SourceRange};
pub use trace::{NoopTrace, TraceEvent, TraceEventKind, TraceSink, VecTrace};
