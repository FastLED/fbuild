//! Integration tests for the `fbuild ci` subcommand (FastLED/fbuild#242).
//!
//! These spawn the compiled `fbuild` binary and assert clap-level argument
//! validation: missing required args produce usage errors. The flag-mapping
//! contract itself is covered by the inline parse tests in
//! `main.rs::ci_tests` -- faster, and avoids a known stack-overflow when
//! debug Windows builds render `--help` for the full subcommand tree.

use std::process::Command;

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
