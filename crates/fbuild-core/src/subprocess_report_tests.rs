//! Tests for [`super::run_command_reporting`] (FastLED/fbuild#1971).
//!
//! The contract under test is the one `run_command` does not offer: after a
//! deadline fires, the child is stopped *and* both streams it already wrote
//! come back with the report. The fake children are shell scripts, so the
//! spawn-driven cells return early on Windows (same shape as
//! `fbuild-deploy`'s containment tests); the bounded-tail logic is covered
//! without a spawn.

use std::path::Path;
use std::time::Duration;

use super::*;

/// Install an executable script without ever exec'ing a file that is open for
/// writing (a sibling test thread forking mid-write would hit ETXTBSY).
fn write_script(path: &Path, body: &str) {
    let staging = path.with_extension("staging");
    std::fs::write(&staging, body).unwrap();
    crate::platform::fs::set_executable(&staging).unwrap();
    std::fs::rename(&staging, path).unwrap();
}

/// A child that exits 0 with output on both streams reports success and both
/// streams.
#[tokio::test]
async fn success_reports_both_streams() {
    if crate::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let script = tmp.path().join("ok.sh");
    write_script(
        &script,
        "#!/bin/sh\necho 'out line'\necho 'err line' >&2\nexit 0\n",
    );
    let report = run_command_reporting(
        &[script.to_str().unwrap()],
        Some(tmp.path()),
        None,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    assert!(report.success(), "{report:?}");
    assert_eq!(report.exit_code, Some(0));
    assert!(!report.timed_out);
    assert_eq!(report.stdout.trim(), "out line");
    assert_eq!(report.stderr.trim(), "err line");
    assert!(!report.stdout_truncated);
}

/// A non-zero exit is data: the caller needs the code and the streams, not an
/// opaque error.
#[tokio::test]
async fn failure_keeps_the_exit_code_and_streams() {
    if crate::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let script = tmp.path().join("fail.sh");
    write_script(
        &script,
        "#!/bin/sh\necho 'compiling'\necho 'CMake Error: boom' >&2\nexit 2\n",
    );
    let report = run_command_reporting(
        &[script.to_str().unwrap()],
        None,
        None,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    assert!(!report.success());
    assert_eq!(report.exit_code, Some(2));
    assert!(!report.timed_out);
    assert!(report.stderr.contains("CMake Error: boom"), "{report:?}");
}

/// The whole point: a child that outlives its budget is stopped, and the
/// output it produced before the deadline survives into the report.
#[tokio::test]
async fn timeout_stops_the_child_and_keeps_what_it_printed() {
    if crate::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let script = tmp.path().join("hang.sh");
    write_script(
        &script,
        "#!/bin/sh\necho 'before the deadline'\necho 'partial error' >&2\nsleep 30\n",
    );
    let started = std::time::Instant::now();
    let report = run_command_reporting(
        &[script.to_str().unwrap()],
        None,
        None,
        Duration::from_millis(400),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    assert!(report.timed_out, "{report:?}");
    assert_eq!(report.exit_code, None);
    assert!(!report.success());
    assert!(report.stdout.contains("before the deadline"), "{report:?}");
    assert!(report.stderr.contains("partial error"), "{report:?}");
    assert!(
        elapsed < Duration::from_secs(20),
        "timeout was not bounded: {elapsed:?}"
    );
}

/// Each stream is bounded to its tail, and the report still states how much
/// the child wrote (the truncation is visible, not silent).
#[tokio::test]
async fn streams_are_bounded_to_a_tail_and_report_the_total() {
    if crate::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let script = tmp.path().join("chatty.sh");
    write_script(
        &script,
        "#!/bin/sh\nprintf 'HEAD'\ni=0\nwhile [ $i -lt 200 ]; do printf '0123456789'; i=$((i+1)); done\nprintf 'TAIL'\n",
    );
    let report = run_command_reporting_with_limit(
        &[script.to_str().unwrap()],
        None,
        None,
        Duration::from_secs(30),
        64,
    )
    .await
    .unwrap();
    assert!(report.success(), "{report:?}");
    assert!(report.stdout_truncated, "{report:?}");
    assert_eq!(report.stdout_bytes, 4 + 2000 + 4);
    // The rendered string carries the layout's trailing newline; the retained
    // bytes are bounded at 64.
    assert!(
        report.stdout.trim_end_matches('\n').len() <= 64,
        "retained {} bytes",
        report.stdout.len()
    );
    assert!(
        report.stdout.trim_end().ends_with("TAIL"),
        "tail was not kept: {:?}",
        report.stdout
    );
}

/// A spawn that cannot even start is an error, not an empty success.
#[tokio::test]
async fn missing_program_is_an_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let missing = tmp.path().join("does-not-exist");
    let error = run_command_reporting(
        &[missing.to_str().unwrap()],
        None,
        None,
        Duration::from_secs(5),
    )
    .await
    .expect_err("a missing program must not report success");
    assert!(
        !matches!(error, crate::FbuildError::Timeout(_)),
        "{error:?}"
    );
}
