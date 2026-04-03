//! Centralized build output log.
//!
//! Collects build output lines and optionally streams them in real-time
//! through a channel sender. Uses `VecDeque` internally to reduce memory
//! fragmentation on repeated pushes.

use std::collections::VecDeque;
use std::sync::mpsc::Sender;

/// Centralized build output log.
///
/// Accumulates build output lines (compilation steps, warnings, size info, etc.)
/// and optionally streams each line through a channel for real-time delivery
/// from the daemon to the CLI.
pub struct BuildLog {
    lines: VecDeque<String>,
    sender: Option<Sender<String>>,
}

impl BuildLog {
    /// Create a log that only collects lines locally.
    pub fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            sender: None,
        }
    }

    /// Create a log that streams each line through the given sender.
    pub fn with_sender(sender: Sender<String>) -> Self {
        Self {
            lines: VecDeque::new(),
            sender: Some(sender),
        }
    }

    /// Push a line. If a sender is configured, also streams it immediately.
    pub fn push(&mut self, line: impl Into<String>) {
        let line = line.into();
        if let Some(ref sender) = self.sender {
            // Best-effort send; if receiver is dropped, silently ignore
            let _ = sender.send(line.clone());
        }
        self.lines.push_back(line);
    }

    /// Consume the log and return all collected lines.
    pub fn into_lines(self) -> Vec<String> {
        self.lines.into()
    }

    /// Check if the log is empty.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

impl Default for BuildLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_collect() {
        let mut log = BuildLog::new();
        log.push("line 1");
        log.push("line 2".to_string());
        let lines = log.into_lines();
        assert_eq!(lines, vec!["line 1", "line 2"]);
    }

    #[test]
    fn empty_log() {
        let log = BuildLog::new();
        assert!(log.is_empty());
        assert!(log.into_lines().is_empty());
    }

    #[test]
    fn streams_through_sender() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut log = BuildLog::with_sender(tx);
        log.push("hello");
        log.push("world");
        assert_eq!(rx.try_recv().unwrap(), "hello");
        assert_eq!(rx.try_recv().unwrap(), "world");
        // Lines are also kept locally
        let lines = log.into_lines();
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn sender_dropped_does_not_panic() {
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        let mut log = BuildLog::with_sender(tx);
        // Should not panic even though receiver is gone
        log.push("orphaned line");
        assert_eq!(log.into_lines(), vec!["orphaned line"]);
    }
}
