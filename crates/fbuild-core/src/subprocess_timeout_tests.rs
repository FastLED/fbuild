//! FastLED/fbuild#1867: a timed-out command must not leave writers behind.
//!
//! `wait_and_capture` returned `FbuildError::Timeout` the moment the deadline
//! expired. Dropping the Tokio child stops that child only: the compiler
//! driver's own descendants (`cc` -> `collect2` -> `ld`) kept running and kept
//! writing the artifact the caller had already been told had failed.
//!
//! The regression command borrows the real `Esp32Linker` shape — a driver that
//! forks a worker which writes the output later — but needs no toolchain and no
//! hardware: `sh` forks a delayed writer and then sits idle until the deadline.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::platform::host;
use crate::platform::process::{Termination, init_containment, terminate_tree};
use crate::subprocess::{run_command, run_command_passthrough};
use crate::{FbuildError, Result};

/// A scratch directory under the crate's own target tree (`ban_unrooted_tempdir`).
fn scratch(label: &str) -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("subprocess-timeout-tests");
    tempfile::TempDir::new_in(&base)
        .or_else(|_| {
            std::fs::create_dir_all(&base)?;
            tempfile::tempdir_in(&base)
        })
        .unwrap_or_else(|err| panic!("scratch dir for {label}: {err}"))
}

/// How long the aborted command's descendant waits before writing its marker.
const DESCENDANT_WRITE_DELAY: &str = "0.3";
/// How long the direct child lingers so the deadline fires first.
const DIRECT_CHILD_LINGER_SECS: u32 = 30;
/// The command deadline: shorter than both delays above.
const COMMAND_TIMEOUT: Duration = Duration::from_millis(250);
/// Polling budget for a late write from a survivor. Generous on purpose: a
/// loaded host may schedule the survivor late, and a survivor that is dead
/// never writes at all.
const SURVIVOR_POLL_BUDGET: Duration = Duration::from_secs(8);

fn linger_script() -> String {
    format!(
        "sh -c 'sleep {DESCENDANT_WRITE_DELAY}; printf late > \"$FBUILD_TIMEOUT_MARKER\"' & \
         sleep {DIRECT_CHILD_LINGER_SECS}"
    )
}

/// Wait out [`SURVIVOR_POLL_BUDGET`] and report whether a late write appeared.
fn late_write_appeared(marker: &Path) -> bool {
    let deadline = Instant::now() + SURVIVOR_POLL_BUDGET;
    while Instant::now() < deadline {
        if marker.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn run_timed_out_capture(marker: &Path, script: &str) -> Result<()> {
    let marker_text = marker.display().to_string();
    let env: [(&str, &str); 1] = [("FBUILD_TIMEOUT_MARKER", marker_text.as_str())];
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime builds");
    runtime
        .block_on(run_command(
            &["sh", "-c", script],
            None,
            Some(&env),
            Some(COMMAND_TIMEOUT),
        ))
        .map(|_| ())
}

#[test]
fn timed_out_capture_command_stops_descendant_writers() {
    if !host::current().is_unix() {
        return;
    }
    init_containment("fbuild-core-test").expect("containment initializes");
    let dir = scratch("capture");
    let marker = dir.path().join("late-write.txt");

    let error = run_timed_out_capture(&marker, &linger_script()).expect_err("deadline must fail");

    assert!(
        matches!(error, FbuildError::Timeout(_)),
        "expected a timeout error, got {error:?}"
    );
    assert!(
        !late_write_appeared(&marker),
        "a descendant of the timed-out command survived and wrote {:?} \
         (FastLED/fbuild#1867)",
        marker
    );
}

#[test]
fn timed_out_passthrough_command_stops_descendant_writers() {
    if !host::current().is_unix() {
        return;
    }
    init_containment("fbuild-core-test").expect("containment initializes");
    let dir = scratch("passthrough");
    let marker = dir.path().join("late-write.txt");

    let marker_text = marker.display().to_string();
    let env: [(&str, &str); 1] = [("FBUILD_TIMEOUT_MARKER", marker_text.as_str())];
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime builds");
    let error = runtime
        .block_on(run_command_passthrough(
            &["sh", "-c", &linger_script()],
            None,
            Some(&env),
            Some(COMMAND_TIMEOUT),
        ))
        .expect_err("deadline must fail");

    assert!(
        matches!(error, FbuildError::Timeout(_)),
        "expected a timeout error, got {error:?}"
    );
    assert!(
        !late_write_appeared(&marker),
        "a descendant of the timed-out passthrough command survived and wrote {:?} \
         (FastLED/fbuild#1867)",
        marker
    );
}

/// The deadline path must not disturb the success path: a command that
/// finishes inside its budget still returns everything it wrote. The payload is
/// far past the 64 KiB pipe buffer, so the streams have to be drained while the
/// child runs instead of only after it exits.
#[test]
fn a_completed_command_still_returns_its_captured_output() {
    if !host::current().is_unix() {
        return;
    }
    init_containment("fbuild-core-test").expect("containment initializes");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime builds");
    let output = runtime
        .block_on(run_command(
            &[
                "sh",
                "-c",
                "yes line | head -n 4096; printf 'stderr-line\\n' >&2",
            ],
            None,
            None,
            Some(Duration::from_secs(60)),
        ))
        .expect("command completes");

    assert_eq!(output.exit_code, 0, "exit code: {}", output.exit_code);
    assert_eq!(
        output.stdout.lines().count(),
        4096,
        "captured {} bytes of stdout",
        output.stdout.len()
    );
    assert_eq!(output.stderr.trim(), "stderr-line");
}

#[test]
fn terminate_tree_of_a_missing_pid_reports_success() {
    init_containment("fbuild-core-test").expect("containment initializes");
    // A pid the host cannot resolve must be a no-op, never a signal to a group
    // the caller does not own.
    assert!(terminate_tree(9_000_000, Termination::Force).is_ok());
}
