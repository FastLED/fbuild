//! Selected Windows host mechanics behind [`crate::platform::host`].

use std::path::PathBuf;

/// `%USERPROFILE%`, falling back to `%HOME%` for environments (MSYS,
/// cross-toolchain shells) that export only the POSIX variable.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}
