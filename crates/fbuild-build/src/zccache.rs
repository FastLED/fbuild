//! Optional zccache compiler cache integration.
//!
//! When zccache is found on PATH, compiler invocations are wrapped as
//! `zccache wrap <real-compiler> <args...>` so that repeated compilations
//! serve cached object files instead of re-invoking gcc/g++.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use fbuild_core::Result;

/// Cached result of searching for zccache on PATH.
static ZCCACHE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

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

/// Find the zccache binary.
///
/// Resolution order:
/// 1. `VIRTUAL_ENV/Scripts/zccache.exe` (Windows) or `VIRTUAL_ENV/bin/zccache` (Unix)
/// 2. `.venv` in ancestor directories (for daemons spawned without VIRTUAL_ENV)
/// 3. Next to the current executable (normal `pip install fbuild` puts both in Scripts/)
/// 4. First match on PATH
pub fn find_zccache() -> Option<&'static Path> {
    ZCCACHE_PATH
        .get_or_init(|| {
            // Allow disabling zccache via environment variable
            if std::env::var("FBUILD_NO_ZCCACHE").is_ok() {
                tracing::info!("zccache disabled via FBUILD_NO_ZCCACHE");
                return None;
            }

            let exe_name = if cfg!(windows) {
                "zccache.exe"
            } else {
                "zccache"
            };

            // 1. Prefer the virtual environment's zccache (set by `uv run`)
            if let Some(venv) = std::env::var_os("VIRTUAL_ENV") {
                let venv_dir = PathBuf::from(venv);
                let bin_dir = if cfg!(windows) {
                    venv_dir.join("Scripts")
                } else {
                    venv_dir.join("bin")
                };
                let candidate = bin_dir.join(exe_name);
                if candidate.is_file() {
                    tracing::info!("found zccache in VIRTUAL_ENV at {}", candidate.display());
                    return Some(candidate);
                }
            }

            // 2. Walk up from cwd looking for .venv (handles daemons without VIRTUAL_ENV)
            if let Ok(cwd) = std::env::current_dir() {
                if let Some(found) = find_zccache_in_venv(&cwd, exe_name) {
                    return Some(found);
                }
            }

            // 3. Sibling of current executable (normal package install)
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    let candidate = dir.join(exe_name);
                    if candidate.is_file() {
                        tracing::info!(
                            "found zccache next to executable at {}",
                            candidate.display()
                        );
                        return Some(candidate);
                    }
                }
            }

            // 4. Fallback: search PATH
            std::env::var_os("PATH").and_then(|path_var| {
                std::env::split_paths(&path_var).find_map(|dir| {
                    let candidate = dir.join(exe_name);
                    if candidate.is_file() {
                        tracing::info!("found zccache on PATH at {}", candidate.display());
                        Some(candidate)
                    } else {
                        None
                    }
                })
            })
        })
        .as_deref()
}

/// Walk up from `start` looking for a `.venv` directory containing zccache.
fn find_zccache_in_venv(start: &Path, exe_name: &str) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let venv = dir.join(".venv");
        if venv.is_dir() {
            let bin_dir = if cfg!(windows) {
                venv.join("Scripts")
            } else {
                venv.join("bin")
            };
            let candidate = bin_dir.join(exe_name);
            if candidate.is_file() {
                tracing::info!("found zccache in .venv at {}", candidate.display());
                return Some(candidate);
            }
        }
        dir = dir.parent()?;
    }
}

/// Start the zccache daemon if it's not already running.
///
/// This is idempotent — `zccache start` is a no-op when the daemon is up.
pub fn ensure_running(zccache: &Path) {
    // INTENTIONALLY DETACHED (FastLED/fbuild#32): zccache is itself a
    // long-running daemon with independent lifecycle management. `start`
    // is a no-op when it's already running, and either way the zccache
    // daemon must survive the fbuild daemon — so this spawn stays out
    // of the containment group.
    // allow-direct-spawn: zccache daemon must outlive the fbuild daemon.
    let mut cmd = std::process::Command::new(zccache);
    cmd.arg("start")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let result = cmd.status();

    match result {
        Ok(status) if status.success() => {
            tracing::info!("zccache daemon running");
        }
        Ok(status) => {
            tracing::warn!("zccache start exited with {}", status);
        }
        Err(e) => {
            tracing::warn!("failed to start zccache daemon: {}", e);
        }
    }
}

/// Prepend zccache explicit-wrap mode to a compiler command line.
///
/// Transforms `["gcc", "-c", "foo.c", ...]` into
/// `["zccache", "wrap", "gcc", "-c", "foo.c", ...]`.
/// If `cache_path` is None, returns the original args unchanged.
pub fn wrap_args(args: &[&str], cache_path: Option<&Path>) -> Vec<String> {
    match cache_path {
        Some(zcc) => {
            let mut wrapped = Vec::with_capacity(args.len() + 2);
            wrapped.push(zcc.to_string_lossy().to_string());
            wrapped.push("wrap".to_string());
            wrapped.extend(args.iter().map(|s| s.to_string()));
            wrapped
        }
        None => args.iter().map(|s| s.to_string()).collect(),
    }
}

/// Return the workspace root to use as the CWD for zccache-wrapped compiles.
///
/// Upstream zccache normalizes cache-key paths relative to the wrapper
/// process CWD. fbuild object files live under `<workspace>/.fbuild/...`, so
/// running the wrapper from `<workspace>` lets identical renamed workspaces
/// share per-TU cache keys even when compiler args contain absolute paths.
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
    if !path.is_absolute() {
        return path.to_string_lossy().to_string();
    }

    // Both ends must be in the same normal form (stripped of any `\\?\`
    // Windows extended-length prefix) for `strip_prefix` to match, since
    // `canonicalize_existing_path` now strips that prefix from `path`.
    let stable_path = canonicalize_existing_path(path).unwrap_or_else(|| path.to_path_buf());
    let stable_cwd = strip_unc_prefix(cwd.to_path_buf());
    stable_path
        .strip_prefix(&stable_cwd)
        .unwrap_or(&stable_path)
        .to_string_lossy()
        .to_string()
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

/// Ask zccache whether the watched root changed since the last successful mark.
///
/// Exit code semantics come from `zccache fp check`:
/// - `0`: changed, build work should run
/// - `1`: unchanged, the watched root can be reused
pub fn check_fingerprint(zccache: &Path, watch: &FingerprintWatch) -> Result<FingerprintCheck> {
    if let Some(parent) = watch.cache_file.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut args = vec![
        zccache.to_string_lossy().to_string(),
        "fp".to_string(),
        "--cache-file".to_string(),
        watch.cache_file.to_string_lossy().to_string(),
        "check".to_string(),
        "--root".to_string(),
        watch.root.to_string_lossy().to_string(),
    ];
    for ext in &watch.extensions {
        args.push("--ext".to_string());
        args.push(ext.clone());
    }
    for exclude in &watch.excludes {
        args.push("--exclude".to_string());
        args.push(exclude.clone());
    }

    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let result = fbuild_core::subprocess::run_command(&args_ref, None, None, None)?;
    match result.exit_code {
        0 => Ok(FingerprintCheck::Changed),
        1 => Ok(FingerprintCheck::Unchanged),
        code => Err(fbuild_core::FbuildError::BuildFailed(format!(
            "zccache fp check failed for {} (exit={}): {}{}",
            watch.root.display(),
            code,
            result.stderr,
            result.stdout
        ))),
    }
}

/// Mark a previously checked watch as successful.
pub fn mark_fingerprint_success(zccache: &Path, watch: &FingerprintWatch) -> Result<()> {
    if let Some(parent) = watch.cache_file.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let args = [
        zccache.to_string_lossy().to_string(),
        "fp".to_string(),
        "--cache-file".to_string(),
        watch.cache_file.to_string_lossy().to_string(),
        "mark-success".to_string(),
    ];
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let result = fbuild_core::subprocess::run_command(&args_ref, None, None, None)?;
    if result.success() {
        Ok(())
    } else {
        Err(fbuild_core::FbuildError::BuildFailed(format!(
            "zccache fp mark-success failed for {}: {}{}",
            watch.root.display(),
            result.stderr,
            result.stdout
        )))
    }
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
            format!("-I{}", vendor.display()),
            format!("--sysroot={}", sysroot.display()),
        ];

        assert_eq!(
            normalize_flags_for_compile_cwd(&flags, &cwd),
            vec![
                "-I".to_string(),
                "include".to_string(),
                "-Ivendor".to_string(),
                "--sysroot=sysroot".to_string(),
            ]
        );
    }
}
