//! Selected macOS host mechanics behind [`crate::platform::host`].

use crate::path::NormalizedPath;

/// `$HOME`.
pub(crate) fn home_dir() -> Option<NormalizedPath> {
    std::env::var_os("HOME").map(NormalizedPath::new)
}
