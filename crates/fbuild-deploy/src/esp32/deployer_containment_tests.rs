//! Containment tests for the external `esptool write-flash` child.
//!
//! FastLED/fbuild#1329: on Windows an ESP32-C6 full flash reached the
//! external esptool child and the daemon disappeared, leaving the client with
//! a dropped connection instead of a flash result. The containment contract
//! is that an external flash-tool exit, timeout, hang or I/O failure stays
//! inside the deploy operation:
//!
//! * the caller gets a structured result (exit status plus the captured
//!   stdout/stderr) instead of an opaque transport error;
//! * a wedged child is bounded by the flash budget, reported as a timeout,
//!   and its process tree is terminated;
//! * a successful child still reports success — that control case is what
//!   proves the fake-child harness reaches the real spawn path.
//!
//! The deployer drives the real `run_command` path here; the only injected
//! part is the `esptool` program itself, which is a shell script. The
//! Windows + ESP32-C6 hardware-in-the-loop cells of #1329 stay open: they
//! need a Windows runner and a real C6 full flash, which this host cannot
//! provide.

#![cfg(test)]

use std::path::Path;
use std::time::Duration;

use fbuild_core::path::NormalizedPath;

use super::deployer::{Esp32Deployer, EsptoolParams};
use super::tests::test_esptool_params;
use super::verify::FlashRegion;
use crate::{DeployOutcome, Deployer};

/// Install an executable script without ever exec'ing a file that is open for
/// writing: a sibling test thread forking mid-write inherits the write fd and
/// exec then fails with ETXTBSY (same pattern as `library/esptool_tests.rs`).
fn write_script(path: &Path, body: &str) {
    let staging = path.with_extension("staging");
    std::fs::write(&staging, body).unwrap();
    fbuild_core::platform::fs::set_executable(&staging).unwrap();
    std::fs::rename(&staging, path).unwrap();
}

fn deployer_with(esptool: &Path, timeout: Duration) -> Esp32Deployer {
    let params: EsptoolParams = test_esptool_params();
    Esp32Deployer::new(
        "esp32c6", "460800", "0x0", "0x8000", "0x10000", &params, false,
    )
    .with_esptool_path(Some(NormalizedPath::new(esptool)))
    .with_flash_timeout(timeout)
}

/// A firmware path that exists, so the argument layout matches production.
///
/// Returned normalized so the module carries no bare `PathBuf` spelling
/// (`ban_std_pathbuf` covers new test modules too).
fn fixture(tmp: &Path) -> NormalizedPath {
    let firmware = tmp.join("firmware.bin");
    std::fs::write(&firmware, [0u8; 32]).unwrap();
    NormalizedPath::new(firmware)
}

/// A normal external flash-tool failure is data for the caller, not an error
/// channel event: exit status and both streams must survive into the result.
#[tokio::test]
async fn failed_flash_child_returns_a_structured_result() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let esptool = tmp.path().join("esptool");
    write_script(
        &esptool,
        "#!/bin/sh\n\
         echo 'fake esptool stdout'\n\
         echo 'A fatal error occurred: Failed to connect to ESP32-C6' >&2\n\
         exit 3\n",
    );
    let firmware = fixture(tmp.path());

    let result = deployer_with(&esptool, Duration::from_secs(30))
        .deploy(tmp.path(), "default", firmware.as_path(), Some("COM13"))
        .await
        .expect("a non-zero child exit must not surface as a transport error");

    assert!(!result.success);
    assert_eq!(result.port.as_deref(), Some("COM13"));
    assert!(
        result.message.contains("exit code 3"),
        "message lost the child exit status: {}",
        result.message
    );
    assert!(
        result.stderr.contains("Failed to connect"),
        "stderr lost the tool diagnostic: {:?}",
        result.stderr
    );
    assert!(
        result.stdout.contains("fake esptool stdout"),
        "stdout was not captured: {:?}",
        result.stdout
    );
    assert!(matches!(result.outcome, DeployOutcome::FullFlash));
}

/// Control case: a child that exits 0 reports success through the same path,
/// so the harness is proven to reach a real spawn rather than a stub.
#[tokio::test]
async fn successful_flash_child_reports_success() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let esptool = tmp.path().join("esptool");
    write_script(
        &esptool,
        "#!/bin/sh\necho 'Hash of data verified.'\nexit 0\n",
    );
    let firmware = fixture(tmp.path());

    let result = deployer_with(&esptool, Duration::from_secs(30))
        .deploy(tmp.path(), "default", firmware.as_path(), Some("COM13"))
        .await
        .unwrap();

    assert!(result.success, "{}", result.message);
    assert!(
        result.stdout.contains("Hash of data verified."),
        "{:?}",
        result.stdout
    );
    assert!(matches!(result.outcome, DeployOutcome::FullFlash));
}

/// A hung child must be bounded by the flash budget, reported as a timeout,
/// and terminated — a survivor would hold the port and block the retry that
/// #1329 asks for.
#[tokio::test]
async fn wedged_flash_child_times_out_and_is_terminated() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let started = tmp.path().join("started");
    let finished = tmp.path().join("finished");
    let esptool = tmp.path().join("esptool");
    write_script(
        &esptool,
        &format!(
            "#!/bin/sh\n: > '{}'\nsleep 30\n: > '{}'\n",
            started.display(),
            finished.display()
        ),
    );
    let firmware = fixture(tmp.path());

    let error = deployer_with(&esptool, Duration::from_millis(700))
        .deploy(tmp.path(), "default", firmware.as_path(), Some("COM13"))
        .await
        .expect_err("a wedged child must not be reported as a completed flash");

    match error {
        fbuild_core::FbuildError::Timeout(message) => assert!(
            message.contains("timed out"),
            "timeout message is not actionable: {message}"
        ),
        other => panic!("expected a timeout, got {other}"),
    }
    assert!(
        started.exists(),
        "the fake child never ran, so the timeout is not evidence"
    );

    // The child is `sleep 30`; if termination failed it would still write the
    // marker well inside this wait.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !finished.exists(),
        "the timed-out child survived its budget and can still hold the port"
    );
}

/// The selective-region rewrite (`deploy_regions`) is the other external
/// esptool call site; it must stay contained exactly like the full flash.
#[tokio::test]
async fn failed_selective_flash_returns_a_structured_result() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let esptool = tmp.path().join("esptool");
    write_script(&esptool, "#!/bin/sh\necho 'write failed' >&2\nexit 7\n");
    let firmware = fixture(tmp.path());

    let result = deployer_with(&esptool, Duration::from_secs(30))
        .deploy_regions(firmware.as_path(), "COM13", &[FlashRegion::Firmware])
        .await
        .expect("a non-zero child exit must not surface as a transport error");

    assert!(!result.success);
    assert!(
        result.message.contains("exit code 7"),
        "message lost the child exit status: {}",
        result.message
    );
    assert!(
        result.stderr.contains("write failed"),
        "{:?}",
        result.stderr
    );
    assert!(matches!(
        result.outcome,
        DeployOutcome::SelectiveFlash { .. }
    ));
}
