//! Emulator runner types shared across fbuild crates.

use serde::{Deserialize, Serialize};

/// Outcome classification for an emulator test run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmulatorOutcome {
    /// Test passed (halt-on-success pattern matched or clean exit with expect satisfied).
    Passed(String),
    /// Test failed (halt-on-error pattern matched or non-zero exit).
    Failed(String),
    /// Emulator crashed (non-zero exit with crash signature detected).
    Crashed(String),
    /// Emulator timed out before a halt pattern matched.
    TimedOut {
        /// Whether the `--expect` pattern was found before timeout.
        expect_found: bool,
    },
    /// The board/platform combination is not supported by any emulator backend.
    Unsupported(String),
}

impl EmulatorOutcome {
    /// Whether this outcome represents a successful run.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Passed(_))
    }
}

impl std::fmt::Display for EmulatorOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Passed(msg) => write!(f, "passed: {}", msg),
            Self::Failed(msg) => write!(f, "failed: {}", msg),
            Self::Crashed(msg) => write!(f, "crashed: {}", msg),
            Self::TimedOut { expect_found } => {
                if *expect_found {
                    write!(f, "timed out (expected pattern was found)")
                } else {
                    write!(f, "timed out (expected pattern NOT found)")
                }
            }
            Self::Unsupported(msg) => write!(f, "unsupported: {}", msg),
        }
    }
}

/// Result of an emulator test run, including captured output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmulatorRunResult {
    /// High-level outcome classification.
    pub outcome: EmulatorOutcome,
    /// Captured stdout from the emulator process.
    pub stdout: String,
    /// Captured stderr from the emulator process.
    pub stderr: String,
    /// The command line that was executed (for diagnostics).
    pub command_line: String,
    /// Process exit code, if the emulator process terminated.
    pub exit_code: Option<i32>,
}

impl EmulatorRunResult {
    /// Convenience: is the outcome a pass?
    pub fn is_success(&self) -> bool {
        self.outcome.is_success()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_display() {
        assert_eq!(
            EmulatorOutcome::Passed("all good".into()).to_string(),
            "passed: all good"
        );
        assert_eq!(
            EmulatorOutcome::Failed("bad".into()).to_string(),
            "failed: bad"
        );
        assert_eq!(
            EmulatorOutcome::Crashed("segfault".into()).to_string(),
            "crashed: segfault"
        );
        assert_eq!(
            EmulatorOutcome::TimedOut { expect_found: true }.to_string(),
            "timed out (expected pattern was found)"
        );
        assert_eq!(
            EmulatorOutcome::TimedOut {
                expect_found: false
            }
            .to_string(),
            "timed out (expected pattern NOT found)"
        );
        assert_eq!(
            EmulatorOutcome::Unsupported("no runner".into()).to_string(),
            "unsupported: no runner"
        );
    }

    #[test]
    fn outcome_is_success() {
        assert!(EmulatorOutcome::Passed("ok".into()).is_success());
        assert!(!EmulatorOutcome::Failed("err".into()).is_success());
        assert!(!EmulatorOutcome::Crashed("crash".into()).is_success());
        assert!(!EmulatorOutcome::TimedOut { expect_found: true }.is_success());
        assert!(!EmulatorOutcome::Unsupported("nope".into()).is_success());
    }

    #[test]
    fn run_result_serde_round_trip() {
        let result = EmulatorRunResult {
            outcome: EmulatorOutcome::Passed("test passed".into()),
            stdout: "Hello from emulator\n".into(),
            stderr: String::new(),
            command_line: "qemu-system-xtensa -nographic ...".into(),
            exit_code: Some(0),
        };
        let json = serde_json::to_string(&result).unwrap();
        let deser: EmulatorRunResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.outcome, result.outcome);
        assert_eq!(deser.stdout, result.stdout);
        assert_eq!(deser.exit_code, Some(0));
    }
}
