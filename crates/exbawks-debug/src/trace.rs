use exbawks_types::{AccessKind, GuestVa};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// One structured emulator trace event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TraceEvent {
    /// The decoder entered one guest basic block.
    BlockEnter {
        /// The first guest instruction address.
        address: GuestVa,
    },
    /// One kernel HLE export was called.
    KernelCall {
        /// The export ordinal.
        ordinal: u16,
        /// The verified export name, when the ordinal table knows it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The guest call site.
        caller: GuestVa,
    },
    /// One graphics method was observed.
    GraphicsMethod {
        /// The method identifier.
        method: u32,
        /// The method data word.
        data: u32,
    },
    /// One guest memory access entered a slow path.
    MemorySlowPath {
        /// The access address.
        address: GuestVa,
        /// The access class.
        access: AccessKind,
        /// The access width in bytes.
        width: u8,
    },
    /// Guest execution stopped.
    Stop {
        /// A stable diagnostic reason.
        reason: String,
    },
}

/// The kind of one trace event, for filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TraceEventKind {
    /// Block-entry events.
    BlockEnter,
    /// Kernel HLE call events.
    KernelCall,
    /// Graphics method events.
    GraphicsMethod,
    /// Memory slow-path events.
    MemorySlowPath,
    /// Execution stop events.
    Stop,
}

impl TraceEvent {
    /// Returns the kind of this event.
    #[must_use]
    pub const fn kind(&self) -> TraceEventKind {
        match self {
            Self::BlockEnter { .. } => TraceEventKind::BlockEnter,
            Self::KernelCall { .. } => TraceEventKind::KernelCall,
            Self::GraphicsMethod { .. } => TraceEventKind::GraphicsMethod,
            Self::MemorySlowPath { .. } => TraceEventKind::MemorySlowPath,
            Self::Stop { .. } => TraceEventKind::Stop,
        }
    }
}

/// A destination for structured trace events.
pub trait TraceSink: Send + Sync {
    /// Records one trace event.
    fn record(&self, event: TraceEvent);
}

/// A trace sink that discards all events.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTrace;

impl TraceSink for NoopTrace {
    fn record(&self, _event: TraceEvent) {}
}

/// An in-memory trace sink for tests and tools.
#[derive(Debug, Default)]
pub struct VecTrace {
    events: RwLock<Vec<TraceEvent>>,
}

impl VecTrace {
    /// Returns a snapshot of all events.
    #[must_use]
    pub fn snapshot(&self) -> Vec<TraceEvent> {
        self.events.read().clone()
    }

    /// Removes and returns all events.
    #[must_use]
    pub fn drain(&self) -> Vec<TraceEvent> {
        std::mem::take(&mut *self.events.write())
    }

    /// Returns the current event count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.read().len()
    }

    /// Returns true when no events exist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.read().is_empty()
    }
}

impl TraceSink for VecTrace {
    fn record(&self, event: TraceEvent) {
        self.events.write().push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_trace_records_and_drains() {
        let trace = VecTrace::default();
        trace.record(TraceEvent::BlockEnter { address: GuestVa(0x1000) });

        assert_eq!(trace.len(), 1);
        assert_eq!(trace.drain().len(), 1);
        assert!(trace.is_empty());
    }
}
