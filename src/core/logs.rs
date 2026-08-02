use std::{collections::VecDeque, sync::Mutex};

use chrono::{DateTime, Utc};
use serde::Serialize;

const MAX_LINES: usize = 2_000;

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub stream: String,
    pub message: String,
}

#[derive(Debug, Default)]
struct Inner {
    next: u64,
    entries: VecDeque<LogEntry>,
}

#[derive(Debug, Default)]
pub struct LogBuffer(Mutex<Inner>);

impl LogBuffer {
    pub fn append(&self, stream: impl Into<String>, message: impl Into<String>) {
        let mut inner = self.0.lock().expect("log buffer mutex poisoned");
        inner.next += 1;
        let sequence = inner.next;
        inner.entries.push_back(LogEntry {
            sequence,
            timestamp: Utc::now(),
            stream: stream.into(),
            message: message.into(),
        });
        if inner.entries.len() > MAX_LINES {
            inner.entries.pop_front();
        }
    }

    pub fn after(&self, sequence: u64, limit: usize) -> (Vec<LogEntry>, u64) {
        let inner = self.0.lock().expect("log buffer mutex poisoned");
        let mut entries: Vec<_> = inner
            .entries
            .iter()
            .filter(|entry| entry.sequence > sequence)
            .cloned()
            .collect();
        if limit > 0 && entries.len() > limit {
            entries.drain(..entries.len() - limit);
        }
        let next = entries
            .last()
            .map_or(inner.next.max(sequence), |entry| entry.sequence);
        (entries, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_entries_after_cursor() {
        let logs = LogBuffer::default();
        logs.append("system", "one");
        logs.append("stdout", "two");
        let (entries, next) = logs.after(1, 500);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "two");
        assert_eq!(next, 2);
    }

    #[test]
    fn limits_history_to_latest_lines() {
        let logs = LogBuffer::default();
        for index in 0..2_100 {
            logs.append("stdout", index.to_string());
        }
        let (entries, next) = logs.after(0, 0);
        assert_eq!(entries.len(), MAX_LINES);
        assert_eq!(entries[0].sequence, 101);
        assert_eq!(next, 2_100);
    }
}
