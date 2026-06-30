//! Embedded-mode zccache integration.
//!
//! [FastLED/fbuild#789](https://github.com/FastLED/fbuild/issues/789)
//! Phase 4 stage 2 (#800): the wrapper-binary path is gone. The
//! managed `zccache` binary download, `find_zccache` resolution,
//! `wrap_args` command-line rewriting, `ensure_running` /
//! `stop` daemon lifecycle, and the protocol-mismatch retry
//! recovery are all deleted. Per-compile dispatch and per-watch
//! fingerprint checks always go through
//! [`crate::zccache_embedded`].
//!
//! Closes FastLED/fbuild#32 — the rationale for the intentionally-
//! detached `zccache start` spawn was that the wrapper daemon had
//! to outlive `fbuild-daemon`. Embedded mode means the cache IS
//! `fbuild-daemon`, so there's nothing to detach.
//!
//! This module retains:
//! - [`FingerprintWatch`] / [`FingerprintCheck`] — the public types
//!   build-fingerprint consumers pass in/out.
//! - [`check_fingerprint`] / [`mark_fingerprint_success`] — thin
//!   wrappers around the embedded fingerprint API.
//! - Path-normalization helpers ([`compile_cwd_from_output`],
//!   [`path_arg_for_compile_cwd`], [`normalize_flags_for_compile_cwd`])
//!   — still used by `compile_source` to keep cache keys
//!   workspace-relative.

use std::path::{Path, PathBuf};

use fbuild_core::{FbuildError, Result};

/// A persistent zccache fingerprint watch.
#[derive(Debug, Clone)]
pub struct FingerprintWatch {
    pub cache_file: PathBuf,
    pub root: PathBuf,
    pub extensions: Vec<String>,
    pub excludes: Vec<String>,
}

/// Result of a zccache fingerprint check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintCheck {
    Changed,
    Unchanged,
}

/// Ask the embedded fingerprint engine whether the watched root
/// changed since the last successful mark.
///
/// Drives [`crate::zccache_embedded::check_fingerprint_embedded`]
/// directly. The wrapper-binary path was deleted in #800.
pub fn check_fingerprint(watch: &FingerprintWatch) -> Result<FingerprintCheck> {
    use crate::zccache_embedded::EmbeddedFingerprintCheck;
    match crate::zccache_embedded::check_fingerprint_embedded(
        &watch.cache_file,
        &watch.root,
        &watch.extensions,
        &watch.excludes,
    ) {
        Ok(EmbeddedFingerprintCheck::Changed) => Ok(FingerprintCheck::Changed),
        Ok(EmbeddedFingerprintCheck::Unchanged) => Ok(FingerprintCheck::Unchanged),
        Err(err) => Err(FbuildError::BuildFailed(format!(
            "embedded fingerprint check failed for {}: {err}",
            watch.root.display(),
        ))),
    }
}

/// Mark a previously checked watch as successful.
///
/// Drives [`crate::zccache_embedded::mark_fingerprint_success_embedded`]
/// directly.
pub fn mark_fingerprint_success(watch: &FingerprintWatch) -> Result<()> {
    crate::zccache_embedded::mark_fingerprint_success_embedded(&watch.cache_file).map_err(|err| {
        FbuildError::BuildFailed(format!(
            "embedded fingerprint mark-success failed for {}: {err}",
            watch.root.display(),
        ))
    })
}

/// Return the workspace root to use as the CWD for zccache compiles.
///
/// Upstream zccache normalizes cache-key paths relative to the
/// compile CWD. fbuild object files live under
/// `<workspace>/.fbuild/...`, so running the compile from
/// `<workspace>` lets identical renamed workspaces share per-TU
/// cache keys even when compiler args contain absolute paths.
pub fn compile_cwd_from_output(output: &Path) -> Option<PathBuf> {
    let mut dir = output.parent()?;
    loop {
        if dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(".fbuild"))
        {
            return dir.parent().map(|workspace| {
                canonicalize_existing_path(workspace).unwrap_or_else(|| workspace.to_path_buf())
            });
        }
        dir = dir.parent()?;
    }
}

/// Return a path argument that is stable relative to the zccache compile CWD.
///
/// macOS can canonicalize `/var/...` working directories to `/private/var/...`
/// inside the child process. Canonicalizing absolute compiler arguments before
/// stripping the compile CWD keeps zccache keys workspace-relative across both
/// path spellings.
pub fn path_arg_for_compile_cwd(path: &Path, cwd: &Path) -> String {
    let raw = if !path.is_absolute() {
        path.to_string_lossy().to_string()
    } else {
        // Both ends must be in the same normal form (stripped of any `\\?\`
        // Windows extended-length prefix) for `strip_prefix` to match, since
        // `canonicalize_existing_path` now strips that prefix from `path`.
        let stable_path = canonicalize_existing_path(path).unwrap_or_else(|| path.to_path_buf());
        let stable_cwd = strip_unc_prefix(cwd.to_path_buf());
        let relative = stable_path
            .strip_prefix(&stable_cwd)
            .unwrap_or(&stable_path)
            .to_string_lossy()
            .to_string();
        if relative.is_empty() {
            ".".to_string()
        } else {
            relative
        }
    };
    // FastLED/fbuild#875 follow-up: GCC's internal spec-file pass (the
    // temp file the driver uses to hand args to cc1/cc1plus) treats `\`
    // as an escape character — so `src\main.cpp` reaches cc1plus as
    // `srcmain.cpp` and fails with "fatal error: srcmain.cpp: No such
    // file or directory". Surfaced as soon as the env fix in #885 let
    // the spec-file mechanism actually create its temp file. Forward
    // slashes are unambiguous on every GCC port (Windows GCC resolves
    // both against the FS), so the safe Windows compile contract is to
    // spell every path argument with `/`. POSIX hosts pass through.
    if cfg!(windows) {
        raw.replace('\\', "/")
    } else {
        raw
    }
}

/// Normalize common path-bearing compiler flags for a zccache CWD.
pub fn normalize_flags_for_compile_cwd(flags: &[String], cwd: &Path) -> Vec<String> {
    let mut normalized = Vec::with_capacity(flags.len());
    let mut next_is_path = false;

    for flag in flags {
        if next_is_path {
            normalized.push(path_arg_for_compile_cwd(Path::new(flag), cwd));
            next_is_path = false;
            continue;
        }

        if flag_takes_path_argument(flag) {
            normalized.push(flag.clone());
            next_is_path = true;
            continue;
        }

        if let Some(value) = flag.strip_prefix("--sysroot=") {
            normalized.push(format!(
                "--sysroot={}",
                path_arg_for_compile_cwd(Path::new(value), cwd)
            ));
            continue;
        }

        if let Some((prefix, value)) = split_joined_path_flag(flag) {
            normalized.push(format!(
                "{}{}",
                prefix,
                path_arg_for_compile_cwd(Path::new(value), cwd)
            ));
            continue;
        }

        normalized.push(flag.clone());
    }

    normalized
}

fn canonicalize_existing_path(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(strip_unc_prefix(canonical));
    }

    let parent = path.parent()?.canonicalize().ok()?;
    let joined = match path.file_name() {
        Some(name) => parent.join(name),
        None => parent,
    };
    Some(strip_unc_prefix(joined))
}

/// On Windows, `Path::canonicalize` returns paths prefixed with the
/// extended-length namespace marker `\\?\`. That prefix only works with
/// backslash path separators, but the response-file writer rewrites all
/// backslashes to forward slashes (so GCC doesn't interpret them as escape
/// sequences). The result is `//?/C:/…` which neither GCC nor mingw's path
/// search code understands as a valid drive-letter path, so `#include`
/// lookups against canonicalized include dirs silently fail (see FastLED
/// issue #2507 — `soc/soc_caps.h` not found on esp32p4).
///
/// Stripping the prefix here turns `\\?\C:\…` into `C:\…`, which survives
/// the backslash→slash rewrite as the standard `C:/…` form GCC accepts.
/// The trade-off is that long-path support (>260 chars) is lost for the
/// stripped paths, but the cache root is `<home>/.fbuild` which is well
/// under the limit in practice.
fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return path;
    }
    let s = path.to_string_lossy();
    // Handle both `\\?\C:\…` and (defensively) the already-rewritten `//?/C:/…`.
    if let Some(rest) = s.strip_prefix(r"\\?\").or_else(|| s.strip_prefix("//?/")) {
        return PathBuf::from(rest);
    }
    path
}

fn flag_takes_path_argument(flag: &str) -> bool {
    matches!(
        flag,
        "-I" | "-isystem"
            | "-iquote"
            | "-idirafter"
            | "-include"
            | "-imacros"
            | "-isysroot"
            | "--sysroot"
    )
}

fn split_joined_path_flag(flag: &str) -> Option<(&'static str, &str)> {
    for prefix in [
        "-I",
        "-isystem",
        "-iquote",
        "-idirafter",
        "-include",
        "-imacros",
        "-isysroot",
    ] {
        if let Some(value) = flag.strip_prefix(prefix).filter(|value| !value.is_empty()) {
            return Some((prefix, value));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_cwd_from_output_uses_workspace_before_fbuild() {
        let output = Path::new("/work/project/.fbuild/build/env/release/src/main.o");

        assert_eq!(
            compile_cwd_from_output(output).as_deref(),
            Some(Path::new("/work/project"))
        );
    }

    #[test]
    fn compile_cwd_from_output_returns_none_without_fbuild_component() {
        let output = Path::new("/work/project/build/env/main.o");

        assert!(compile_cwd_from_output(output).is_none());
    }

    #[test]
    fn compile_cwd_from_output_canonicalizes_existing_workspace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path().join("project");
        let output = workspace.join(".fbuild/build/main.o");
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        // On Windows, canonicalize() yields a `\\?\` extended-length prefix
        // that the response-file writer cannot use (forward-slash rewrite
        // produces `//?/` which gcc rejects). The helper now strips that
        // prefix on Windows, so the expected value must match.
        let expected = strip_unc_prefix(workspace.canonicalize().unwrap());

        assert_eq!(
            compile_cwd_from_output(&output).as_deref(),
            Some(expected.as_path())
        );
    }

    #[test]
    #[cfg(windows)]
    fn strip_unc_prefix_removes_extended_length_marker() {
        let raw = std::path::PathBuf::from(r"\\?\C:\Users\test\.fbuild\cache");
        let stripped = strip_unc_prefix(raw);
        assert_eq!(
            stripped,
            std::path::PathBuf::from(r"C:\Users\test\.fbuild\cache")
        );
    }

    #[test]
    #[cfg(windows)]
    fn strip_unc_prefix_is_idempotent_for_already_normal_paths() {
        let raw = std::path::PathBuf::from(r"C:\Users\test\include");
        let stripped = strip_unc_prefix(raw.clone());
        assert_eq!(stripped, raw);
    }

    #[test]
    fn path_arg_for_compile_cwd_returns_workspace_relative_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("project");
        let source = cwd.join("src/main.cpp");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "int main() { return 0; }\n").unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let expected = Path::new("src")
            .join("main.cpp")
            .to_string_lossy()
            .to_string();

        assert_eq!(path_arg_for_compile_cwd(&source, &cwd), expected);
    }

    #[test]
    fn path_arg_for_compile_cwd_returns_dot_for_workspace_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = cwd.canonicalize().unwrap();

        assert_eq!(path_arg_for_compile_cwd(&cwd, &cwd), ".");
    }

    #[test]
    fn normalize_flags_for_compile_cwd_rewrites_include_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("project");
        let include = cwd.join("include");
        let vendor = cwd.join("vendor");
        let sysroot = cwd.join("sysroot");
        std::fs::create_dir_all(&include).unwrap();
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::create_dir_all(&sysroot).unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let flags = vec![
            "-I".to_string(),
            include.to_string_lossy().to_string(),
            "-I".to_string(),
            cwd.to_string_lossy().to_string(),
            format!("-I{}", vendor.display()),
            format!("-I{}", cwd.display()),
            format!("--sysroot={}", sysroot.display()),
        ];

        assert_eq!(
            normalize_flags_for_compile_cwd(&flags, &cwd),
            vec![
                "-I".to_string(),
                "include".to_string(),
                "-I".to_string(),
                ".".to_string(),
                "-Ivendor".to_string(),
                "-I.".to_string(),
                "--sysroot=sysroot".to_string(),
            ]
        );
    }
}
