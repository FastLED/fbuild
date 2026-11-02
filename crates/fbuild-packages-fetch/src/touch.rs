//! Once-per-process DiskCache LRU touch guard.
//!
//! `PackageBase::is_cached` refreshes the DiskCache LRU when an install hits
//! the cache, so background GC cannot treat a package that is in active use
//! as the oldest unused entry and delete it mid-build (FastLED/fbuild#1341).
//! Resolving one package from dozens of transitive dependencies would rewrite
//! the same index row just as many times, so the touch is keyed and claimed
//! once per process.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::disk_cache;

static PACKAGE_TOUCHES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

pub(crate) fn package_touch_key(
    kind: disk_cache::Kind,
    cache_key: &str,
    version: &str,
    install_path: &Path,
) -> String {
    format!(
        "{}|{}|{}|{}",
        kind.as_str(),
        cache_key,
        version,
        install_path.display()
    )
}

/// Claim `key` for this process. `true` means this caller must do the touch.
pub(crate) fn mark_package_touch_needed(key: String) -> bool {
    let touched = PACKAGE_TOUCHES.get_or_init(|| Mutex::new(HashSet::new()));
    let mut touched = touched.lock().unwrap_or_else(|e| e.into_inner());
    touched.insert(key)
}

#[cfg(test)]
pub(crate) fn clear_package_touch_cache_for_tests() {
    if let Some(touched) = PACKAGE_TOUCHES.get() {
        touched.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}
