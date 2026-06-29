//! Integration tests for the `fbuild ci` subcommand (FastLED/fbuild#242).
//!
//! These spawn the compiled `fbuild` binary and assert the clap surface:
//! `--help` documents the PIO-compatible flags and required-arg validation
//! produces usage errors. The inline parse tests in `main.rs::ci_tests`
//! cover positive-parse + mutual-exclusion contracts at unit-test speed.

use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// Run a `Command` with a hard wall-clock budget (FastLED/fbuild#806).
///
/// `Command::output()` blocks indefinitely if the spawned process wedges
/// (e.g. a regression in clap parsing → infinite loop). 10 s is overkill
/// for `--help` / argument-validation paths, but keeps the test runner
/// from sitting on its 6 h job budget if the CLI ever does wedge.
fn run_cli_or_timeout(mut cmd: Command) -> Output {
    let budget = Duration::from_secs(10);
    let mut child = cmd.spawn().expect("spawn fbuild");
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child.wait_with_output().expect("wait_with_output");
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("fbuild child did not exit within {budget:?} — #806 timeout");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("try_wait error: {e}"),
        }
    }
}

#[test]
fn ci_help_lists_pio_compat_flags() {
    let bin = env!("CARGO_BIN_EXE_fbuild");
    // allow-direct-spawn: integration test driver invoking the compiled fbuild binary.
    let mut cmd = Command::new(bin);
    cmd.args(["ci", "--help"]);
    let output = run_cli_or_timeout(cmd);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ci --help should exit 0\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
    for needle in [
        "--board",
        "--lib",
        "--project-conf",
        "--keep-build-dir",
        "--build-dir",
    ] {
        assert!(
            stdout.contains(needle),
            "help should document {}. got:\n{}",
            needle,
            stdout
        );
    }
}

#[test]
fn ci_without_board_is_a_usage_error() {
    let bin = env!("CARGO_BIN_EXE_fbuild");
    // allow-direct-spawn: integration test driver.
    let mut cmd = Command::new(bin);
    cmd.args(["ci", "examples/Blink/Blink.ino"]);
    let output = run_cli_or_timeout(cmd);

    assert!(
        !output.status.success(),
        "ci without --board must exit non-zero.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn ci_without_sketches_is_a_usage_error() {
    let bin = env!("CARGO_BIN_EXE_fbuild");
    // allow-direct-spawn: integration test driver.
    let mut cmd = Command::new(bin);
    cmd.args(["ci", "--board", "uno"]);
    let output = run_cli_or_timeout(cmd);

    assert!(
        !output.status.success(),
        "ci with no positional sketches must exit non-zero.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
