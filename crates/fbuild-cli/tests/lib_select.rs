//! Unit-level tests for the `fbuild lib-select` diagnostic subcommand.
//!
//! These exercise just the CLI surface: argument parsing, conflict
//! resolution between `--explain`/`--json`, and the early-exit paths that
//! don't require a populated framework cache. Heavier end-to-end tests
//! that need a real framework install (Teensy, STM32, ...) are gated
//! behind `#[ignore]` and run manually.

use std::process::Command;

/// `fbuild lib-select --help` must exit 0 and document both modes.
#[test]
fn lib_select_help_lists_command() {
    let bin = env!("CARGO_BIN_EXE_fbuild");
    // allow-direct-spawn: integration test driver invoking the compiled fbuild binary.
    let output = Command::new(bin)
        .args(["lib-select", "--help"])
        .output()
        .expect("spawn fbuild");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "lib-select --help should exit 0\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("--explain"),
        "help should document --explain. got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("--json"),
        "help should document --json. got:\n{}",
        stdout
    );
}

/// `fbuild lib-select <bogus path>` must exit non-zero. We don't care about
/// the precise code, only that callers (CI, scripts) can detect the error.
#[test]
fn lib_select_missing_project_exits_nonzero() {
    let bin = env!("CARGO_BIN_EXE_fbuild");
    // allow-direct-spawn: integration test driver invoking the compiled fbuild binary.
    let output = Command::new(bin)
        .args([
            "lib-select",
            "/this/path/should/not/exist/and/never/will",
            "-e",
            "uno",
        ])
        .output()
        .expect("spawn fbuild");

    let code = output.status.code().unwrap_or(-1);
    assert_ne!(
        code,
        0,
        "lib-select on a missing project must exit non-zero.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// `--explain` and `--json` are mutually exclusive (clap `conflicts_with`).
/// Passing both must fail at argument-parse time, not silently pick one.
#[test]
fn lib_select_explain_and_json_conflict() {
    let bin = env!("CARGO_BIN_EXE_fbuild");
    // allow-direct-spawn: integration test driver invoking the compiled fbuild binary.
    let output = Command::new(bin)
        .args(["lib-select", ".", "--explain", "--json"])
        .output()
        .expect("spawn fbuild");

    assert!(
        !output.status.success(),
        "--explain + --json must be a usage error.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // clap's conflict message contains "cannot be used with" — assert on
    // that string so we catch a regression where the conflict gets dropped
    // and one flag silently wins.
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("conflict"),
        "expected clap conflict message in stderr.\nstderr: {}",
        stderr
    );
}
