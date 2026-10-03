//! Windows runs APE images natively (an APE file is a valid PE), so there is
//! nothing to extract or cache.

use std::path::PathBuf;

pub(crate) fn default_loader_dirs() -> Vec<PathBuf> {
    Vec::new()
}

pub(crate) fn anonymous_executable(_bytes: &[u8], _name: &str) -> Option<PathBuf> {
    None
}
