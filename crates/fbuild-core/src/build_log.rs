//! Centralized build output log.
//!
//! Collects build output lines and optionally streams them in real-time
//! through a channel sender. Uses `VecDeque` internally to reduce memory
//! fragmentation on repeated pushes.
//!
//! When an `epoch` is set, every line pushed is automatically prefixed with
//! the elapsed time since that epoch (e.g. `"   0.46 compiling foo.cpp"`).

use crate::channel::UnboundedSender;
use std::collections::VecDeque;
use std::time::Instant;

/// Centralized build output log.
///
/// Accumulates build output lines (compilation steps, warnings, size info, etc.)
/// and optionally streams each line through a channel for real-time delivery
/// from the daemon to the CLI.
///
/// The streaming sender is a `tokio::sync::mpsc::UnboundedSender` so that
/// push-from-sync-code and recv-from-async-code share one channel without an
/// intermediate `spawn_blocking` bridge — `UnboundedSender::send` is sync and
/// callable from any context (see fbuild#818 async-audit follow-up).
pub struct BuildLog {
    lines: VecDeque<String>,
    sender: Option<UnboundedSender<String>>,
    epoch: Option<Instant>,
}

impl BuildLog {
    /// Create a log that only collects lines locally (no timestamps).
    pub fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            sender: None,
            epoch: None,
        }
    }

    /// Create a log that streams each line through the given sender (no timestamps).
    pub fn with_sender(sender: UnboundedSender<String>) -> Self {
        Self {
            lines: VecDeque::new(),
            sender: Some(sender),
            epoch: None,
        }
    }

    /// Create a log with elapsed-time prefixes from the given epoch.
    pub fn with_epoch(epoch: Instant) -> Self {
        Self {
            lines: VecDeque::new(),
            sender: None,
            epoch: Some(epoch),
        }
    }

    /// Create a log that streams each line and prefixes with elapsed time.
    pub fn with_sender_and_epoch(sender: UnboundedSender<String>, epoch: Instant) -> Self {
        Self {
            lines: VecDeque::new(),
            sender: Some(sender),
            epoch: Some(epoch),
        }
    }

    /// Push a line. If an epoch is set, the line is prefixed with elapsed time.
    /// If a sender is configured, also streams the (possibly prefixed) line immediately.
    pub fn push(&mut self, line: impl Into<String>) {
        let line = line.into();
        let tagged = match self.epoch {
            Some(epoch) => {
                let prefix = crate::elapsed::format_elapsed(epoch.elapsed());
                format!("{prefix}{line}")
            }
            None => line,
        };
        if let Some(ref sender) = self.sender {
            let _ = sender.send(tagged.clone());
        }
        self.lines.push_back(tagged);
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
    use std::time::Duration;

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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
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
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        drop(rx);
        let mut log = BuildLog::with_sender(tx);
        // Should not panic even though receiver is gone
        log.push("orphaned line");
        assert_eq!(log.into_lines(), vec!["orphaned line"]);
    }

    // --- Phase 2 TDD: epoch integration ---

    #[test]
    fn push_with_epoch_prepends_elapsed() {
        let epoch = Instant::now();
        std::thread::sleep(Duration::from_millis(50));
        let mut log = BuildLog::with_epoch(epoch);
        log.push("hello");
        let lines = log.into_lines();
        assert!(lines[0].ends_with("hello"), "line: {:?}", lines[0]);
        assert!(lines[0].len() > "hello".len(), "no prefix added");
        assert!(!lines[0].contains('['), "should not contain brackets");
    }

    #[test]
    fn push_without_epoch_no_prefix() {
        let mut log = BuildLog::new();
        log.push("hello");
        assert_eq!(log.into_lines(), vec!["hello"]);
    }

    #[test]
    fn with_sender_and_epoch_streams_prefixed() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let epoch = Instant::now();
        let mut log = BuildLog::with_sender_and_epoch(tx, epoch);
        log.push("test");
        let received = rx.try_recv().unwrap();
        assert!(received.ends_with("test"), "received: {:?}", received);
        assert!(received.len() > "test".len(), "no prefix in streamed line");
    }
}
