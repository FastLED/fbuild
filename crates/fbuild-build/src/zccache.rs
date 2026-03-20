//! Optional zccache compiler cache integration.
//!
//! When zccache is found on PATH, compiler invocations are wrapped as
//! `zccache <real-compiler> <args...>` so that repeated compilations
//! serve cached object files instead of re-invoking gcc/g++.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Cached result of searching for zccache on PATH.
static ZCCACHE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Find the zccache binary on PATH (cached after first call).
pub fn find_zccache() -> Option<&'static Path> {
    ZCCACHE_PATH
        .get_or_init(|| {
            let exe_name = if cfg!(windows) {
                "zccache.exe"
            } else {
                "zccache"
            };

            std::env::var_os("PATH").and_then(|path_var| {
                std::env::split_paths(&path_var).find_map(|dir| {
                    let candidate = dir.join(exe_name);
                    if candidate.is_file() {
                        tracing::info!("found zccache at {}", candidate.display());
                        Some(candidate)
                    } else {
                        None
                    }
                })
            })
        })
        .as_deref()
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
