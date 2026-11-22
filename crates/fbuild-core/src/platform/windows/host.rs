//! Selected Windows host mechanics behind [`crate::platform::host`].

use crate::path::NormalizedPath;

/// `%USERPROFILE%`, falling back to `%HOME%` for environments (MSYS,
/// cross-toolchain shells) that export only the POSIX variable.
pub(crate) fn home_dir() -> Option<NormalizedPath> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(NormalizedPath::new)
}

/// Windows has no load average; callers treat `None` as "unknown".
pub(crate) fn load_average_1m() -> Option<f64> {
    None
}
