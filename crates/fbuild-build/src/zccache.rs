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
            return dir.parent().map(Path::to_path_buf);
        }
        dir = dir.parent()?;
    }
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
}
