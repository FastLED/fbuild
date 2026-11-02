//! macOS pieces of APE support: default cache directories. macOS has no
//! anonymous-executable fallback (no memfd), so a usable cache directory is
//! required; `$TMPDIR` is already per-user there.

use crate::path::NormalizedPath;

/// Default candidate directories, most durable first.
pub(crate) fn default_loader_dirs() -> Vec<NormalizedPath> {
    let env_dir = |key: &str| {
        std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map(NormalizedPath::new)
    };
    let mut dirs = Vec::new();
    if let Some(cache) = env_dir("XDG_CACHE_HOME") {
        dirs.push(cache.join("fbuild").join("ape"));
    }
    if let Some(home) = env_dir("HOME") {
        dirs.push(home.join("Library").join("Caches").join("fbuild").join("ape"));
    }
    dirs
}

pub(crate) fn anonymous_executable(_bytes: &[u8], _name: &str) -> Option<NormalizedPath> {
    None
}

pub(crate) fn is_anonymous(_path: &std::path::Path) -> bool {
    false
}
