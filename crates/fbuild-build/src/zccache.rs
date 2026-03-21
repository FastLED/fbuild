//! Optional zccache compiler cache integration.
//!
//! When zccache is found on PATH, compiler invocations are wrapped as
//! `zccache <real-compiler> <args...>` so that repeated compilations
//! serve cached object files instead of re-invoking gcc/g++.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Cached result of searching for zccache on PATH.
static ZCCACHE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

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
    let result = std::process::Command::new(zccache)
        .arg("start")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

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

/// Prepend zccache to a compiler command line.
///
/// Transforms `["gcc", "-c", "foo.c", ...]` into `["zccache", "gcc", "-c", "foo.c", ...]`.
/// If `cache_path` is None, returns the original args unchanged.
pub fn wrap_args(args: &[&str], cache_path: Option<&Path>) -> Vec<String> {
    match cache_path {
        Some(zcc) => {
            let mut wrapped = Vec::with_capacity(args.len() + 1);
            wrapped.push(zcc.to_string_lossy().to_string());
            wrapped.extend(args.iter().map(|s| s.to_string()));
            wrapped
        }
        None => args.iter().map(|s| s.to_string()).collect(),
    }
}
