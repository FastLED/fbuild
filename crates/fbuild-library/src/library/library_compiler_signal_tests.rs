//! A signal-killed compiler must say so (FastLED/fbuild#1628 ask 3, #1630).
//!
//! `-9` with empty stderr used to be the whole story, which reads like a
//! compiler crash instead of a kill.

use std::path::Path;

use super::{compile_exit_error, compile_failure_message};

#[test]
fn a_signal_killed_compiler_is_reported_as_a_kill() {
    let killed = compile_failure_message(Path::new("/src/a.cpp"), "fastled", -9, "");
    if fbuild_core::platform::host::is_unix() {
        assert!(
            killed.contains("exit code -9 = SIGKILL (signal 9)"),
            "{killed}"
        );
        assert!(killed.contains("killed by a signal"), "{killed}");
        assert!(
            killed.contains("not a source or compiler error"),
            "{killed}"
        );
    } else {
        // Windows collapses every abnormal exit to -1 and keeps no signal.
        assert!(!killed.contains("SIGKILL"), "{killed}");
    }
    assert!(killed.contains("wrote nothing to stderr"), "{killed}");
}

/// A tool that shells out reports `128 + signal` (zccache forwards 137 for a
/// SIGKILLed child), so that spelling must be named too.
#[test]
fn a_shell_convention_kill_code_is_named() {
    let killed = compile_failure_message(Path::new("/src/a.cpp"), "fastled", 137, "");
    if fbuild_core::platform::host::is_unix() {
        assert!(
            killed.contains("exit code 137 = SIGKILL (signal 9)"),
            "{killed}"
        );
    }
}

/// A kill is carried as its own error kind, so callers that remember failures
/// (the framework-library negative cache) can tell it from a source error and
/// leave it uncached — otherwise a retry replays the kill
/// (FastLED/fbuild#1628 ask 2, #1630).
#[test]
fn a_signal_kill_is_a_termination_error_and_a_source_error_is_not() {
    let killed = compile_exit_error(Path::new("/src/a.cpp"), "fastled", -9, "");
    if fbuild_core::platform::host::is_unix() {
        assert!(killed.is_signal_kill(), "{killed}");
    }
    let source_error =
        compile_exit_error(Path::new("/src/a.cpp"), "fastled", 1, "a.cpp:1: error: x");
    assert!(!source_error.is_signal_kill(), "{source_error}");
}
