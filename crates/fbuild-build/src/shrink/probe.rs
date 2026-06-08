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
use std::path::PathBuf;

use fbuild_core::subprocess::run_command_with_stdin;

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

/// [`Preprocessor`] backed by an external GCC-style cross-compiler invoked
/// via [`std::process::Command`] (FastLED/fbuild#493, #502).
///
/// The probe submits [`PROBE_SOURCE`] on the compiler's stdin with
/// `-E -x c -`, discards stdout, and captures stderr. A non-zero exit code
/// from a `#error` directive is the expected success signal — the trait's
/// fail-closed contract reserves [`io::Error`] for subprocess failures
/// (binary missing, stdin write failed, etc.).
///
/// Targets GCC-family cross-compilers (`xtensa-esp-elf-gcc`,
/// `arm-none-eabi-gcc`, `avr-gcc`, ...). Clang's GCC-compatible driver
/// also accepts the same flag set; MSVC's `cl.exe` does not and is out of
/// scope for this probe.
#[derive(Debug, Clone)]
pub struct ExternalPreprocessor {
    compiler: PathBuf,
    extra_args: Vec<String>,
}

impl ExternalPreprocessor {
    /// Construct a preprocessor that will invoke the binary at `compiler`.
    ///
    /// `compiler` may be an absolute path, a relative path, or a bare
    /// name (in which case it is resolved via `PATH` by the OS).
    pub fn new(compiler: impl Into<PathBuf>) -> Self {
        Self {
            compiler: compiler.into(),
            extra_args: Vec::new(),
        }
    }

    /// Append extra arguments to be passed before `-E -x c -` on every
    /// invocation. Useful for `-target=...` / `-isysroot ...` / similar
    /// cross-toolchain plumbing.
    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extra_args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Path to the underlying compiler binary.
    #[must_use]
    pub fn compiler(&self) -> &std::path::Path {
        &self.compiler
    }

    /// Extra arguments appended to every invocation.
    #[must_use]
    pub fn extra_args(&self) -> &[String] {
        &self.extra_args
    }
}

impl Preprocessor for ExternalPreprocessor {
    fn preprocess(&self, source: &str) -> io::Result<PreprocessResult> {
        // Route through `fbuild_core::subprocess::run_command_with_stdin` so
        // the child process inherits fbuild's containment / pipe-buffer
        // handling rather than calling `std::process::Command` directly
        // (forbidden by the `ci/find_direct_subprocess.py` lint, see
        // FastLED/fbuild#141).
        let compiler_str = self.compiler.to_string_lossy().into_owned();
        let mut args: Vec<&str> = Vec::with_capacity(self.extra_args.len() + 5);
        args.push(compiler_str.as_str());
        for extra in &self.extra_args {
            args.push(extra.as_str());
        }
        args.extend_from_slice(&["-E", "-x", "c", "-"]);

        let output = run_command_with_stdin(&args, source.as_bytes(), None, None, None)
            .map_err(|e| io::Error::other(e.to_string()))?;

        Ok(PreprocessResult {
            // `run_command_with_stdin` collapses signal-killed cases into a
            // synthetic exit code, so this is always `Some(_)` — the
            // `Option` here is purely for forward-compatibility with
            // [`Preprocessor`] implementations that distinguish the two.
            exit_code: Some(output.exit_code),
            stderr: output.stderr,
        })
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

    #[test]
    fn external_preprocessor_constructor_stores_compiler_path() {
        let pp = ExternalPreprocessor::new("/usr/bin/gcc");
        assert_eq!(pp.compiler(), std::path::Path::new("/usr/bin/gcc"));
        assert!(pp.extra_args().is_empty());
    }

    #[test]
    fn external_preprocessor_with_args_appends() {
        let pp = ExternalPreprocessor::new("cc")
            .with_args(["-target", "thumb-none-eabi"])
            .with_args(["-isysroot", "/opt/sdk"]);
        assert_eq!(
            pp.extra_args(),
            &["-target", "thumb-none-eabi", "-isysroot", "/opt/sdk"],
        );
    }

    #[test]
    fn external_preprocessor_missing_compiler_is_io_error() {
        let pp =
            ExternalPreprocessor::new("definitely-not-a-real-compiler-binary-fastled-fbuild-502");
        let r = pp.preprocess(PROBE_SOURCE);
        assert!(
            r.is_err(),
            "expected I/O error when compiler binary is missing; got {r:?}",
        );
    }

    #[test]
    fn probe_libc_fails_closed_on_missing_compiler() {
        // End-to-end fail-closed check: missing toolchain must resolve to
        // Libc::Unknown, never panic, never claim a libc.
        let pp =
            ExternalPreprocessor::new("definitely-not-a-real-compiler-binary-fastled-fbuild-502");
        assert_eq!(probe_libc(&pp), Libc::Unknown);
    }

    /// Locate a host C compiler on PATH. Returns the first of
    /// `cc` / `gcc` / `clang` that responds to `--version`.
    ///
    /// On GitHub Actions runners (`ubuntu-latest`, `macos-latest`,
    /// `windows-latest` with MSYS2/MinGW) at least one is preinstalled.
    ///
    /// Routes through `fbuild_core::subprocess::run_command` rather than
    /// invoking `std::process::Command` directly — the `ban_raw_subprocess`
    /// dylint forbids raw spawns even for benign `--version` probes.
    fn find_host_cc() -> Option<&'static str> {
        use fbuild_core::subprocess::run_command;
        for candidate in ["cc", "gcc", "clang"] {
            let args = [candidate, "--version"];
            if matches!(
                run_command(&args, None, None, Some(std::time::Duration::from_secs(5))),
                Ok(o) if o.success()
            ) {
                return Some(candidate);
            }
        }
        None
    }

    #[test]
    fn host_cc_smoke_completes_preprocess_without_io_error() {
        let Some(cc) = find_host_cc() else {
            eprintln!("test skipped: no host C compiler (cc/gcc/clang) on PATH");
            return;
        };
        let pp = ExternalPreprocessor::new(cc);
        let result = pp
            .preprocess(PROBE_SOURCE)
            .expect("subprocess wiring should not error on a real host compiler");

        // The libc the host ships isn't constrained by this test — it could
        // be glibc, musl, BSD libc (macOS), MSVCRT/UCRT (MSYS2/MinGW), or
        // newlib (Cygwin). All of those preprocess cleanly with our probe
        // source. We only assert that the subprocess delivered an exit
        // code and stderr that probe_libc can classify.
        assert!(
            result.exit_code.is_some(),
            "expected a real exit code from the host preprocessor",
        );

        let libc = probe_libc(&pp);
        // Any classification is acceptable as long as no panic / I/O error
        // bubbled up; we just exercise the end-to-end wiring.
        let _ = libc;
    }
}
