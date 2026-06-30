//! Unit-level tests for the `fbuild lib-select` diagnostic subcommand.
//!
//! These exercise just the CLI surface: argument parsing, conflict
//! resolution between `--explain`/`--json`, and the early-exit paths that
//! don't require a populated framework cache. Heavier end-to-end tests
//! that need a real framework install (Teensy, STM32, ...) are gated
//! behind `#[ignore]` and run manually.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Run a `Command` with a hard wall-clock budget (FastLED/fbuild#806).
///
/// `Command::output()` blocks indefinitely if the spawned process wedges
/// (e.g. a regression in clap parsing → infinite loop). 10 s is overkill
/// for `--help` / argument-validation paths, but keeps the test runner
/// from sitting on its 6 h job budget if the CLI ever does wedge.
///
/// stdout/stderr must be piped explicitly: `spawn()` inherits the parent
/// streams by default, so without this `wait_with_output()` reports an
/// empty stdout and the `--help`-content assertions panic spuriously.
/// `Command::output()` pipes for you; `spawn()` does not.
fn run_cli_or_timeout(mut cmd: Command) -> Output {
    let budget = Duration::from_secs(10);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
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

/// `fbuild lib-select --help` must exit 0 and document both modes.
#[test]
fn lib_select_help_lists_command() {
    let bin = env!("CARGO_BIN_EXE_fbuild");
    // allow-direct-spawn: integration test driver invoking the compiled fbuild binary.
    let mut cmd = Command::new(bin);
    cmd.args(["lib-select", "--help"]);
    let output = run_cli_or_timeout(cmd);

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
    let mut cmd = Command::new(bin);
    cmd.args([
        "lib-select",
        "/this/path/should/not/exist/and/never/will",
        "-e",
        "uno",
    ]);
    let output = run_cli_or_timeout(cmd);

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
    let mut cmd = Command::new(bin);
    cmd.args(["lib-select", ".", "--explain", "--json"]);
    let output = run_cli_or_timeout(cmd);

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
