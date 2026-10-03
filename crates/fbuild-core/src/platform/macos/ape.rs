//! macOS pieces of APE support: default cache directories. macOS has no
//! anonymous-executable fallback (no memfd), so a usable cache directory is
//! required; `$TMPDIR` is already per-user there.

use std::path::PathBuf;

/// Default candidate directories, most durable first.
pub(crate) fn default_loader_dirs() -> Vec<PathBuf> {
    let env_dir = |key: &str| {
        std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let mut dirs = Vec::new();
    if let Some(cache) = env_dir("XDG_CACHE_HOME") {
        dirs.push(cache.join("fbuild").join("ape"));
    }
    if let Some(home) = env_dir("HOME") {
        dirs.push(home.join("Library").join("Caches").join("fbuild").join("ape"));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    dirs.push(std::env::temp_dir().join(format!("fbuild-ape-{uid}")));
    dirs
}

pub(crate) fn anonymous_executable(_bytes: &[u8], _name: &str) -> Option<PathBuf> {
    None
}
