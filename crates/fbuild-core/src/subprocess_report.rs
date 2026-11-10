//! Bounded, timeout-preserving subprocess capture (FastLED/fbuild#1971).
//!
//! [`super::run_command`] is the workspace's default spawn. On a deadline it
//! returns [`crate::FbuildError::Timeout`] with only the budget in the
//! message: the two streams the child already wrote are dropped with the
//! cancelled capture future. For a delegated build that is the wrong trade —
//! the output *is* the diagnosis, and a caller that wants to report "the build
//! timed out, here is what it printed" cannot get it any other way without
//! spawning its own child.
//!
//! This module adds that variant rather than changing the default: the
//! streams are drained by tasks that own their pipe and write into shared
//! buffers, the deadline applies to the child's exit, and each stream is
//! retained as a bounded tail so a chatty child cannot exhaust memory. The
//! process tree is stopped with the same containment as
//! [`super::run_command`] (FastLED/fbuild#1867), so a timed-out build leaves
//! no writer behind.
//!
//! The buffers are shared rather than returned by the drain task for a
//! measured reason: containment only puts the child in its own process group
//! inside the daemon, so in a test or CLI process a grandchild that survived
//! the kill can hold the pipe open. Snapshotting the buffer means such a
//! straggler costs the *last* bytes, never the report.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;

use super::{
    build_command, bytes_to_lines_string, exit_code_from, spawn_err, stop_timed_out_command,
};
use crate::{FbuildError, Result};

/// Bytes retained per stream, counted on disk-order output: the tail is kept
/// because a compiler's errors are at the end of its output, not its start.
pub const STREAM_CAPTURE_LIMIT: usize = 64 * 1024;

/// How long a drain task is given to reach EOF once the child is gone.
const DRAIN_SETTLE_BUDGET: Duration = Duration::from_millis(500);

/// A finished-or-stopped child, with the streams a timeout would discard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandReport {
    /// Retained tail of standard output (at most `limit` bytes).
    pub stdout: String,
    /// Retained tail of standard error (at most `limit` bytes).
    pub stderr: String,
    /// Exit code, or `None` when the child was stopped for its deadline.
    pub exit_code: Option<i32>,
    /// True when the deadline stopped the child; `exit_code` is then `None`.
    pub timed_out: bool,
    /// Total bytes the child wrote to each stream, before bounding.
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    /// True when a stream exceeded the retained limit and was tail-trimmed.
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl CommandReport {
    /// True only for a child that exited 0 within its budget.
    pub fn success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

/// Run `args` with an explicit deadline, keeping both bounded streams either
/// way.
///
/// Unlike [`super::run_command`] there is no "no timeout" form: a delegated
/// build without a deadline can wedge the daemon indefinitely, which is the
/// failure mode this exists to prevent.
pub async fn run_command_reporting(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Duration,
) -> Result<CommandReport> {
    run_command_reporting_with_limit(args, cwd, env, timeout, STREAM_CAPTURE_LIMIT).await
}

/// [`run_command_reporting`] with an explicit per-stream retain limit, so a
/// test can prove truncation without writing 64 KiB.
pub async fn run_command_reporting_with_limit(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Duration,
    limit: usize,
) -> Result<CommandReport> {
    if args.is_empty() {
        return Err(FbuildError::Other("empty command".to_string()));
    }
    let mut cmd = build_command(
        args, cwd, env, /*capture=*/ true, /*stdin_piped=*/ false,
    )?;
    let mut child = crate::platform::process::spawn_tokio_contained(&mut cmd)
        .map_err(|e| spawn_err(args, e))?;
    let pid = child.id().unwrap_or(0);

    // The drain tasks own their pipes, so a deadline that fires below cannot
    // cancel the read. `wait` is the only thing under the timeout.
    let stdout_sink = Arc::new(Mutex::new(Captured::default()));
    let stderr_sink = Arc::new(Mutex::new(Captured::default()));
    let stdout_task = tokio::spawn(drain_into(
        child.stdout.take(),
        limit,
        Arc::clone(&stdout_sink),
    ));
    let stderr_task = tokio::spawn(drain_into(
        child.stderr.take(),
        limit,
        Arc::clone(&stderr_sink),
    ));

    let deadline = tokio::time::timeout(timeout, child.wait()).await;
    let (exit_code, timed_out) = match deadline {
        Ok(Ok(status)) => (Some(exit_code_from(status)), false),
        Ok(Err(error)) => {
            stdout_task.abort();
            stderr_task.abort();
            return Err(FbuildError::Other(format!(
                "command {args:?} failed: {error}"
            )));
        }
        Err(_) => {
            stop_timed_out_command(&mut child, pid).await;
            (None, true)
        }
    };

    let (stdout, stdout_bytes) = settle_stream(stdout_task, &stdout_sink).await;
    let (stderr, stderr_bytes) = settle_stream(stderr_task, &stderr_sink).await;

    Ok(CommandReport {
        stdout: bytes_to_lines_string(&stdout),
        stderr: bytes_to_lines_string(&stderr),
        exit_code,
        timed_out,
        stdout_bytes,
        stderr_bytes,
        stdout_truncated: stdout_bytes > stdout.len() as u64,
        stderr_truncated: stderr_bytes > stderr.len() as u64,
    })
}

/// Give a drain task a moment to reach EOF, then take whatever the shared
/// buffer holds even if a survivor still owns the pipe.
async fn settle_stream(
    mut task: tokio::task::JoinHandle<()>,
    sink: &Arc<Mutex<Captured>>,
) -> (Vec<u8>, u64) {
    if tokio::time::timeout(DRAIN_SETTLE_BUDGET, &mut task)
        .await
        .is_err()
    {
        task.abort();
    }
    snapshot(sink)
}

/// Read the retained tail and total byte count out of a shared buffer.
fn snapshot(sink: &Arc<Mutex<Captured>>) -> (Vec<u8>, u64) {
    match sink.lock() {
        Ok(captured) => (captured.kept.clone(), captured.total),
        Err(poisoned) => {
            let captured = poisoned.into_inner();
            (captured.kept.clone(), captured.total)
        }
    }
}

/// What a drain task keeps: the last `limit` bytes, plus everything counted.
#[derive(Debug, Default)]
struct Captured {
    kept: Vec<u8>,
    total: u64,
}

/// Read a stream to EOF, keeping only its last `limit` bytes while counting
/// everything the child wrote.
async fn drain_into<R>(reader: Option<R>, limit: usize, sink: Arc<Mutex<Captured>>)
where
    R: AsyncRead + Unpin,
{
    let Some(mut reader) = reader else {
        return;
    };
    let mut chunk = vec![0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if let Ok(mut captured) = sink.lock() {
                    captured.total += read as u64;
                    captured.kept.extend_from_slice(&chunk[..read]);
                    if captured.kept.len() > limit {
                        let excess = captured.kept.len() - limit;
                        captured.kept.drain(..excess);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "subprocess_report_tests.rs"]
mod subprocess_report_tests;
