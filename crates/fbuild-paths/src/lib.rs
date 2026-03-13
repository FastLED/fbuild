//! Path resolution for fbuild.
//!
//! Single source of truth for all `.fbuild` paths.
//! Respects `FBUILD_DEV_MODE=1` for dev/prod isolation.

use std::path::{Path, PathBuf};

/// Check if running in development mode.
pub fn is_dev_mode() -> bool {
    std::env::var("FBUILD_DEV_MODE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Root fbuild directory: `~/.fbuild/{dev|prod}`
pub fn get_fbuild_root() -> PathBuf {
    let home = dirs_next().expect("could not determine home directory");
    let mode = if is_dev_mode() { "dev" } else { "prod" };
    home.join(".fbuild").join(mode)
}

/// Daemon files directory.
pub fn get_daemon_dir() -> PathBuf {
    get_fbuild_root().join("daemon")
}

/// Global cache root (or `FBUILD_CACHE_DIR` override).
pub fn get_cache_root() -> PathBuf {
    if let Ok(dir) = std::env::var("FBUILD_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    get_fbuild_root().join("cache")
}

/// Project-local `.fbuild` directory.
pub fn get_project_fbuild_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".fbuild")
}

/// Project build root.
pub fn get_project_build_root(project_dir: &Path) -> PathBuf {
    get_project_fbuild_dir(project_dir).join("build")
}

/// Daemon port: 8865 (dev) or 8765 (prod).
pub fn get_daemon_port() -> u16 {
    if is_dev_mode() {
        8865
    } else {
        8765
    }
}

/// Daemon URL.
pub fn get_daemon_url() -> String {
    format!("http://127.0.0.1:{}", get_daemon_port())
}

fn dirs_next() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_mode_port() {
        // Note: can't set env vars in parallel tests safely,
        // so just test the function exists and returns a valid port
        let port = get_daemon_port();
        assert!(port == 8765 || port == 8865);
    }
}
