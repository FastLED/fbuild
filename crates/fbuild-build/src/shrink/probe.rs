//! Fail-closed libc detection (FastLED/fbuild#493, #500).
//!
//! The `--shrink=auto` resolver needs to know which libc a given toolchain
//! ships so it can decide whether the printf-thin shadow archive will be a
//! net win (newlib) or a no-op / net loss (picolibc — the libc is already
//! optimal, ESP-IDF 6.x default).
//!
//! Detection works by submitting a tiny TU to the toolchain's preprocessor:
//!
//! ```text
//! #ifdef __PICOLIBC__
//! #error PICOLIBC
//! #endif
//! #ifdef __NEWLIB__
//! #error NEWLIB
//! #endif
//! ```
//!
//! and inspecting its stderr for either marker. The probe is fail-closed:
//! any I/O failure, missing toolchain, or absent marker maps to
//! [`Libc::Unknown`], which downstream code treats as "don't auto-shrink".
//!
//! Phase 1b-i (this file) lands the [`Libc`] enum, the abstract
//! [`Preprocessor`] trait that lets unit tests exercise every branch
//! without a real cross-compiler in the test environment, and the
//! [`probe_libc`] resolver. The concrete `ExternalPreprocessor` impl
//! backed by [`std::process::Command`] lands in a follow-up sub-issue so
//! this PR stays small and fully unit-testable.

use std::io;

/// Detected libc family for a given toolchain.
///
/// The default is [`Libc::Unknown`] — the fail-closed branch that
/// downstream code treats as "don't auto-shrink".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Libc {
    /// Newlib (full or nano variant). Currently the auto-resolver's only
    /// candidate for `--shrink=safe`.
    Newlib,
    /// Picolibc. The libc is already optimal — auto-resolves to
    /// `--shrink=off` because our printf-thin shadow would be net-negative.
    Picolibc,
    /// Probe failed, toolchain shipped an unrecognized libc, or the host
    /// runs glibc/musl (not a real cross target). Auto-resolves to
    /// `--shrink=off`.
    #[default]
    Unknown,
}

/// Result of a single preprocessor invocation.
///
/// `exit_code` is `None` when the process was killed by a signal; on
/// Windows that case does not occur and the field is always `Some`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreprocessResult {
    pub exit_code: Option<i32>,
    pub stderr: String,
}

/// Abstract "invoke a C preprocessor on this source string and capture
/// stderr". Decouples [`probe_libc`] from the concrete subprocess machinery
/// so unit tests can exercise every branch without a real cross-compiler.
///
/// Implementations should run the preprocessor in `-E`-equivalent mode
/// (read from stdin or a temp file, write to /dev/null, capture stderr).
/// A non-zero exit code from a `#error` directive is the expected success
/// signal for the probe — implementations must not treat it as a fatal
/// error.
pub trait Preprocessor {
    /// Preprocess `source` and return the captured result. An [`Err`]
    /// outcome is reserved for I/O failures (compiler binary missing,
    /// stdin write failed, etc.) — a non-zero exit from a `#error`
    /// directive is reported via [`PreprocessResult::exit_code`].
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] when the preprocessor cannot be invoked
    /// or its output cannot be captured. The [`probe_libc`] resolver
    /// treats any error as [`Libc::Unknown`].
    fn preprocess(&self, source: &str) -> io::Result<PreprocessResult>;
}

/// The TU submitted to the preprocessor. Kept as a single static so tests
/// can inspect it via [`PROBE_SOURCE`] if they ever need to.
pub const PROBE_SOURCE: &str = "\
#ifdef __PICOLIBC__
#error PICOLIBC
#endif
#ifdef __NEWLIB__
#error NEWLIB
#endif
";

/// Fail-closed libc probe.
///
/// Submits [`PROBE_SOURCE`] to the given preprocessor and classifies the
/// result:
///
/// * stderr contains `"PICOLIBC"` → [`Libc::Picolibc`]
/// * stderr contains `"NEWLIB"` → [`Libc::Newlib`]
/// * any other outcome (clean compile, I/O error, garbled output) →
///   [`Libc::Unknown`]
///
/// Picolibc wins over newlib when both markers appear — this case isn't a
/// real toolchain configuration (the two libcs do not coexist in one
/// header tree) but precedence is documented for forward compatibility
/// with embedded distros that vendor cross-libc shims.
#[must_use]
pub fn probe_libc<P: Preprocessor + ?Sized>(p: &P) -> Libc {
    let Ok(result) = p.preprocess(PROBE_SOURCE) else {
        return Libc::Unknown;
    };
    if result.stderr.contains("PICOLIBC") {
        Libc::Picolibc
    } else if result.stderr.contains("NEWLIB") {
        Libc::Newlib
    } else {
        Libc::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory preprocessor that returns a canned [`PreprocessResult`].
    struct MockPreprocessor {
        exit_code: Option<i32>,
        stderr: &'static str,
    }

    impl Preprocessor for MockPreprocessor {
        fn preprocess(&self, _: &str) -> io::Result<PreprocessResult> {
            Ok(PreprocessResult {
                exit_code: self.exit_code,
                stderr: self.stderr.to_string(),
            })
        }
    }

    /// Preprocessor that always fails to spawn — emulates a missing
    /// cross-compiler.
    struct FailingPreprocessor;

    impl Preprocessor for FailingPreprocessor {
        fn preprocess(&self, _: &str) -> io::Result<PreprocessResult> {
            Err(io::Error::new(io::ErrorKind::NotFound, "compiler missing"))
        }
    }

    #[test]
    fn picolibc_marker_in_stderr_yields_picolibc() {
        let mock = MockPreprocessor {
            exit_code: Some(1),
            stderr: "probe.c:2:2: error: #error PICOLIBC\n",
        };
        assert_eq!(probe_libc(&mock), Libc::Picolibc);
    }

    #[test]
    fn newlib_marker_in_stderr_yields_newlib() {
        let mock = MockPreprocessor {
            exit_code: Some(1),
            stderr: "probe.c:5:2: error: #error NEWLIB\n",
        };
        assert_eq!(probe_libc(&mock), Libc::Newlib);
    }

    #[test]
    fn clean_compile_yields_unknown() {
        // Host gcc with glibc / musl preprocesses cleanly: no markers fire.
        let mock = MockPreprocessor {
            exit_code: Some(0),
            stderr: String::new().leak(),
        };
        assert_eq!(probe_libc(&mock), Libc::Unknown);
    }

    #[test]
    fn io_error_yields_unknown() {
        // Missing cross-compiler must fail-closed, not panic.
        assert_eq!(probe_libc(&FailingPreprocessor), Libc::Unknown);
    }

    #[test]
    fn both_markers_picolibc_wins() {
        // Not a real toolchain config but the precedence is documented.
        let mock = MockPreprocessor {
            exit_code: Some(1),
            stderr: "probe.c:2: error: #error PICOLIBC\nprobe.c:5: error: #error NEWLIB\n",
        };
        assert_eq!(probe_libc(&mock), Libc::Picolibc);
    }

    #[test]
    fn libc_default_is_unknown() {
        assert_eq!(Libc::default(), Libc::Unknown);
    }

    #[test]
    fn probe_source_contains_both_markers() {
        // Belt-and-suspenders: a future refactor that drops one of the
        // #ifdef branches would silently regress detection on one libc.
        assert!(PROBE_SOURCE.contains("__PICOLIBC__"));
        assert!(PROBE_SOURCE.contains("__NEWLIB__"));
        assert!(PROBE_SOURCE.contains("#error PICOLIBC"));
        assert!(PROBE_SOURCE.contains("#error NEWLIB"));
    }
}
