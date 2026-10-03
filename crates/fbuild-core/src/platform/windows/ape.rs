//! Windows runs APE images natively (an APE file is a valid PE), so there is
//! nothing to extract or cache.

use crate::path::NormalizedPath;

pub(crate) fn default_loader_dirs() -> Vec<NormalizedPath> {
    Vec::new()
}

pub(crate) fn anonymous_executable(_bytes: &[u8], _name: &str) -> Option<NormalizedPath> {
    None
}

pub(crate) fn is_anonymous(_path: &std::path::Path) -> bool {
    false
}
