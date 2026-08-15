use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::{TraceEvent, TraceSink};

/// One serialized trace record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceRecord {
    /// The monotonically increasing record number.
    pub sequence: u64,
    /// The recorded event.
    pub event: TraceEvent,
    /// An optional private host context path.
    ///
    /// Traces omit this field unless the writer opts in, so shared trace
    /// files carry no private paths by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_path: Option<String>,
}

/// A trace sink that writes one JSON object per line.
#[derive(Debug)]
pub struct JsonLinesTrace<W: Write + Send> {
    writer: Mutex<W>,
    sequence: AtomicU64,
    host_path: Option<String>,
    write_errors: AtomicU64,
}

impl<W: Write + Send> JsonLinesTrace<W> {
    /// Creates a writer without private host paths.
    pub fn new(writer: W) -> Self {
        Self {
            writer: Mutex::new(writer),
            sequence: AtomicU64::new(0),
            host_path: None,
            write_errors: AtomicU64::new(0),
        }
    }

    /// Attaches one private host path to every record.
    #[must_use]
    pub fn with_host_path(mut self, host_path: impl Into<String>) -> Self {
        self.host_path = Some(host_path.into());
        self
    }

    /// Returns the number of records that failed to serialize or write.
    #[must_use]
    pub fn write_errors(&self) -> u64 {
        self.write_errors.load(Ordering::Relaxed)
    }

    /// Flushes and returns the underlying writer.
    pub fn into_inner(self) -> W {
        let mut writer = self.writer.into_inner();
        let _ = writer.flush();
        writer
    }
}

impl<W: Write + Send> TraceSink for JsonLinesTrace<W> {
    fn record(&self, event: TraceEvent) {
        let record = TraceRecord {
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            event,
            host_path: self.host_path.clone(),
        };

        let mut writer = self.writer.lock();
        let written = serde_json::to_writer(&mut *writer, &record)
            .map_err(std::io::Error::from)
            .and_then(|()| writeln!(writer));
        if written.is_err() {
            self.write_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use exbawks_types::GuestVa;

    use super::*;

    fn events() -> [TraceEvent; 3] {
        [
            TraceEvent::BlockEnter { address: GuestVa(0x1000) },
            TraceEvent::KernelCall { ordinal: 8, caller: GuestVa(0x1008) },
            TraceEvent::Stop { reason: "GuestExit { code: 0 }".to_owned() },
        ]
    }

    #[test]
    fn every_line_holds_one_sequenced_json_object() {
        let trace = JsonLinesTrace::new(Vec::new());
        for event in events() {
            trace.record(event);
        }

        let bytes = trace.into_inner();
        let text = String::from_utf8(bytes).expect("trace output is UTF-8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);

        for (index, line) in lines.iter().enumerate() {
            let record: TraceRecord =
                serde_json::from_str(line).expect("each line parses as one record");
            assert_eq!(record.sequence, index as u64);
            assert_eq!(record.host_path, None);
        }
    }

    #[test]
    fn host_paths_stay_out_of_default_records() {
        let trace = JsonLinesTrace::new(Vec::new());
        trace.record(TraceEvent::BlockEnter { address: GuestVa(0x1000) });

        let text = String::from_utf8(trace.into_inner()).expect("trace output is UTF-8");
        assert!(!text.contains("host_path"));
    }

    #[test]
    fn host_paths_appear_only_by_opt_in() {
        let trace = JsonLinesTrace::new(Vec::new()).with_host_path("C:/games/private-title.xbe");
        trace.record(TraceEvent::BlockEnter { address: GuestVa(0x1000) });

        let text = String::from_utf8(trace.into_inner()).expect("trace output is UTF-8");
        let record: TraceRecord =
            serde_json::from_str(text.lines().next().expect("one line exists"))
                .expect("the line parses");
        assert_eq!(record.host_path.as_deref(), Some("C:/games/private-title.xbe"));
    }
}
