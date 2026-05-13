//! Integration tests for the `fbuild ci` subcommand (FastLED/fbuild#242).
//!
//! These spawn the compiled `fbuild` binary and assert the clap surface:
//! `--help` documents the PIO-compatible flags and required-arg validation
//! produces usage errors. The inline parse tests in `main.rs::ci_tests`
//! cover positive-parse + mutual-exclusion contracts at unit-test speed.

use std::process::Command;

#[test]
fn ci_help_lists_pio_compat_flags() {
    let bin = env!("CARGO_BIN_EXE_fbuild");
    // allow-direct-spawn: integration test driver invoking the compiled fbuild binary.
    let output = Command::new(bin)
        .args(["ci", "--help"])
        .output()
        .expect("spawn fbuild");

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
    let output = Command::new(bin)
        .args(["ci", "examples/Blink/Blink.ino"])
        .output()
        .expect("spawn fbuild");

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
    let output = Command::new(bin)
        .args(["ci", "--board", "uno"])
        .output()
        .expect("spawn fbuild");

    assert!(
        !output.status.success(),
        "ci with no positional sketches must exit non-zero.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
