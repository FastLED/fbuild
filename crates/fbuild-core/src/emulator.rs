//! Emulator runner types shared across fbuild crates.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Artifact bundle
// ---------------------------------------------------------------------------

/// Machine-readable artifact bundle for emulator consumption.
///
/// Different emulator backends require different subsets of these artifacts:
/// - **SimavrRunner**: requires `firmware_elf`
/// - **Avr8jsRunner**: requires `firmware_hex`
/// - **QemuRunner (ESP32)**: requires `firmware_bin`; optionally `bootloader`,
///   `partitions`, and `firmware_elf` (for crash decoding)
///
/// Use [`EmulatorArtifactBundle::from_build_dir`] to scan a build output
/// directory and populate the bundle automatically.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmulatorArtifactBundle {
    /// Primary firmware binary (`.bin`).
    pub firmware_bin: Option<PathBuf>,
    /// Intel HEX firmware (`.hex`).
    pub firmware_hex: Option<PathBuf>,
    /// ELF with debug symbols (`.elf`).
    pub firmware_elf: Option<PathBuf>,
    /// Bootloader binary (ESP32-family).
    pub bootloader: Option<PathBuf>,
    /// Partition table binary (ESP32-family).
    pub partitions: Option<PathBuf>,
    /// Pre-merged flash image (all sections combined).
    pub merged_image: Option<PathBuf>,
}

/// Which runner backend the bundle is being validated for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerKind {
    Avr8js,
    Simavr,
    QemuEsp32,
}

impl EmulatorArtifactBundle {
    /// Populate a bundle by scanning a build output directory.
    ///
    /// Recognises the canonical file names produced by fbuild's build pipeline:
    /// `firmware.bin`, `firmware.hex`, `firmware.elf`, `bootloader.bin`,
    /// `partitions.bin`, and any `*_merged.bin` image.
    pub fn from_build_dir(dir: &Path) -> Self {
        let mut bundle = Self::default();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return bundle;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            match name {
                "firmware.bin" => bundle.firmware_bin = Some(path),
                "firmware.hex" => bundle.firmware_hex = Some(path),
                "firmware.elf" => bundle.firmware_elf = Some(path),
                "bootloader.bin" => bundle.bootloader = Some(path),
                "partitions.bin" => bundle.partitions = Some(path),
                _ if name.ends_with("_merged.bin") => bundle.merged_image = Some(path),
                _ => {}
            }
        }
        bundle
    }

    /// Build a bundle from explicit firmware and ELF paths (backwards compat).
    pub fn from_paths(firmware_path: &Path, elf_path: Option<&Path>) -> Self {
        let mut bundle = Self::default();
        if let Some(ext) = firmware_path.extension().and_then(|e| e.to_str()) {
            match ext {
                "bin" => bundle.firmware_bin = Some(firmware_path.to_path_buf()),
                "hex" => bundle.firmware_hex = Some(firmware_path.to_path_buf()),
                "elf" => bundle.firmware_elf = Some(firmware_path.to_path_buf()),
                _ => bundle.firmware_bin = Some(firmware_path.to_path_buf()),
            }
        } else {
            bundle.firmware_bin = Some(firmware_path.to_path_buf());
        }
        if let Some(elf) = elf_path {
            bundle.firmware_elf = Some(elf.to_path_buf());
        }
        bundle
    }

    /// Validate that the bundle contains all artifacts required by the given
    /// runner kind. Returns `Ok(())` or an error describing what is missing.
    pub fn validate_for(&self, runner: RunnerKind) -> Result<(), String> {
        match runner {
            RunnerKind::Simavr => self.require_exists("firmware_elf", self.firmware_elf.as_deref()),
            RunnerKind::Avr8js => self.require_exists("firmware_hex", self.firmware_hex.as_deref()),
            RunnerKind::QemuEsp32 => {
                self.require_exists("firmware_bin", self.firmware_bin.as_deref())
            }
        }
    }

    /// List the artifact roles present in this bundle.
    pub fn available_roles(&self) -> Vec<&'static str> {
        let mut roles = Vec::new();
        if self.firmware_bin.is_some() {
            roles.push("firmware_bin");
        }
        if self.firmware_hex.is_some() {
            roles.push("firmware_hex");
        }
        if self.firmware_elf.is_some() {
            roles.push("firmware_elf");
        }
        if self.bootloader.is_some() {
            roles.push("bootloader");
        }
        if self.partitions.is_some() {
            roles.push("partitions");
        }
        if self.merged_image.is_some() {
            roles.push("merged_image");
        }
        roles
    }

    fn require_exists(&self, role: &str, path: Option<&Path>) -> Result<(), String> {
        match path {
            Some(p) if p.exists() => Ok(()),
            Some(p) => Err(format!(
                "artifact bundle has {} path but file does not exist: {}",
                role,
                p.display()
            )),
            None => Err(format!(
                "artifact bundle is missing required artifact: {}",
                role
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Outcome classification
// ---------------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Artifact bundle tests
    // -----------------------------------------------------------------------

    #[test]
    fn bundle_from_build_dir_populates_known_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("firmware.bin"), b"bin").unwrap();
        std::fs::write(dir.path().join("firmware.hex"), b"hex").unwrap();
        std::fs::write(dir.path().join("firmware.elf"), b"elf").unwrap();
        std::fs::write(dir.path().join("bootloader.bin"), b"bl").unwrap();
        std::fs::write(dir.path().join("partitions.bin"), b"pt").unwrap();
        std::fs::write(dir.path().join("app_merged.bin"), b"merged").unwrap();
        // unrelated file should be ignored
        std::fs::write(dir.path().join("compile_commands.json"), b"{}").unwrap();

        let bundle = EmulatorArtifactBundle::from_build_dir(dir.path());
        assert!(bundle.firmware_bin.is_some());
        assert!(bundle.firmware_hex.is_some());
        assert!(bundle.firmware_elf.is_some());
        assert!(bundle.bootloader.is_some());
        assert!(bundle.partitions.is_some());
        assert!(bundle.merged_image.is_some());
        assert_eq!(bundle.available_roles().len(), 6);
    }

    #[test]
    fn bundle_from_build_dir_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let bundle = EmulatorArtifactBundle::from_build_dir(dir.path());
        assert!(bundle.firmware_bin.is_none());
        assert!(bundle.available_roles().is_empty());
    }

    #[test]
    fn bundle_from_build_dir_nonexistent() {
        let bundle = EmulatorArtifactBundle::from_build_dir(Path::new("/no/such/dir"));
        assert!(bundle.available_roles().is_empty());
    }

    #[test]
    fn bundle_from_paths_bin() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = dir.path().join("firmware.bin");
        std::fs::write(&bin, b"bin").unwrap();
        let elf = dir.path().join("firmware.elf");
        std::fs::write(&elf, b"elf").unwrap();

        let bundle = EmulatorArtifactBundle::from_paths(&bin, Some(&elf));
        assert_eq!(bundle.firmware_bin.as_deref(), Some(bin.as_path()));
        assert_eq!(bundle.firmware_elf.as_deref(), Some(elf.as_path()));
        assert!(bundle.firmware_hex.is_none());
    }

    #[test]
    fn bundle_from_paths_hex() {
        let dir = tempfile::TempDir::new().unwrap();
        let hex = dir.path().join("firmware.hex");
        std::fs::write(&hex, b"hex").unwrap();

        let bundle = EmulatorArtifactBundle::from_paths(&hex, None);
        assert_eq!(bundle.firmware_hex.as_deref(), Some(hex.as_path()));
        assert!(bundle.firmware_bin.is_none());
        assert!(bundle.firmware_elf.is_none());
    }

    #[test]
    fn bundle_validate_simavr_requires_elf() {
        let dir = tempfile::TempDir::new().unwrap();
        let elf = dir.path().join("firmware.elf");
        std::fs::write(&elf, b"elf").unwrap();

        let mut bundle = EmulatorArtifactBundle::default();
        assert!(bundle.validate_for(RunnerKind::Simavr).is_err());

        bundle.firmware_elf = Some(elf);
        assert!(bundle.validate_for(RunnerKind::Simavr).is_ok());
    }

    #[test]
    fn bundle_validate_avr8js_requires_hex() {
        let dir = tempfile::TempDir::new().unwrap();
        let hex = dir.path().join("firmware.hex");
        std::fs::write(&hex, b"hex").unwrap();

        let mut bundle = EmulatorArtifactBundle::default();
        assert!(bundle.validate_for(RunnerKind::Avr8js).is_err());

        bundle.firmware_hex = Some(hex);
        assert!(bundle.validate_for(RunnerKind::Avr8js).is_ok());
    }

    #[test]
    fn bundle_validate_qemu_requires_bin() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = dir.path().join("firmware.bin");
        std::fs::write(&bin, b"bin").unwrap();

        let mut bundle = EmulatorArtifactBundle::default();
        assert!(bundle.validate_for(RunnerKind::QemuEsp32).is_err());

        bundle.firmware_bin = Some(bin);
        assert!(bundle.validate_for(RunnerKind::QemuEsp32).is_ok());
    }

    #[test]
    fn bundle_validate_missing_file_on_disk() {
        let bundle = EmulatorArtifactBundle {
            firmware_elf: Some(PathBuf::from("/nonexistent/firmware.elf")),
            ..Default::default()
        };
        let err = bundle.validate_for(RunnerKind::Simavr).unwrap_err();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn bundle_serde_round_trip() {
        let bundle = EmulatorArtifactBundle {
            firmware_bin: Some(PathBuf::from("firmware.bin")),
            firmware_hex: None,
            firmware_elf: Some(PathBuf::from("firmware.elf")),
            bootloader: Some(PathBuf::from("bootloader.bin")),
            partitions: Some(PathBuf::from("partitions.bin")),
            merged_image: None,
        };
        let json = serde_json::to_string(&bundle).unwrap();
        let deser: EmulatorArtifactBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.firmware_bin, bundle.firmware_bin);
        assert_eq!(deser.firmware_elf, bundle.firmware_elf);
        assert_eq!(deser.bootloader, bundle.bootloader);
    }

    // -----------------------------------------------------------------------
    // Classification golden tests
    // -----------------------------------------------------------------------

    /// Helper: build an `EmulatorRunResult` from an outcome.
    fn make_result(
        outcome: EmulatorOutcome,
        stdout: &str,
        exit_code: Option<i32>,
    ) -> EmulatorRunResult {
        EmulatorRunResult {
            outcome,
            stdout: stdout.to_string(),
            stderr: String::new(),
            command_line: "test-cmd".to_string(),
            exit_code,
        }
    }

    #[test]
    fn classification_passed_is_success() {
        let r = make_result(
            EmulatorOutcome::Passed("halt-on-success matched".into()),
            "TEST PASSED\n",
            Some(0),
        );
        assert!(r.is_success());
        assert!(r.outcome.is_success());
    }

    #[test]
    fn classification_failed_is_not_success() {
        let r = make_result(
            EmulatorOutcome::Failed("assertion failed".into()),
            "ASSERTION FAILED at test.cpp:42\n",
            Some(1),
        );
        assert!(!r.is_success());
    }

    #[test]
    fn classification_crashed_is_not_success() {
        let r = make_result(
            EmulatorOutcome::Crashed("abort() was called at PC 0x42002a3c".into()),
            "Guru Meditation Error\nabort() was called at PC 0x42002a3c\n",
            Some(134),
        );
        assert!(!r.is_success());
        assert!(r.outcome.to_string().contains("crashed"));
    }

    #[test]
    fn classification_timeout_is_not_success() {
        let r = make_result(
            EmulatorOutcome::TimedOut {
                expect_found: false,
            },
            "",
            None,
        );
        assert!(!r.is_success());
        assert!(r.outcome.to_string().contains("timed out"));
    }

    #[test]
    fn classification_timeout_with_expect_found() {
        let r = make_result(
            EmulatorOutcome::TimedOut { expect_found: true },
            "Hello from ESP32!\n",
            None,
        );
        assert!(!r.is_success());
        assert!(r.outcome.to_string().contains("expected pattern was found"));
    }

    #[test]
    fn classification_unsupported_is_not_success() {
        let r = make_result(
            EmulatorOutcome::Unsupported("no runner for stm32".into()),
            "",
            None,
        );
        assert!(!r.is_success());
        assert!(r.outcome.to_string().contains("unsupported"));
    }
}
