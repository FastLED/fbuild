//! Selected macOS host mechanics behind [`crate::platform::host`].

use std::path::PathBuf;

/// `$HOME`.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
