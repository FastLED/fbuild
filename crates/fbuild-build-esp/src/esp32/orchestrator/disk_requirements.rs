//! Disk-space requirements the ESP32 build hands to the engine pre-flight check
//! (FastLED/fbuild#1750).
//!
//! The check itself lives in `fbuild_build_engine::disk_preflight`; this module
//! owns the ESP32 numbers, because only this crate knows which roots an ESP32
//! build writes and how much room a cold one needs. The estimate is
//! deliberately conservative and rounded up: a pre-flight check that passes and
//! then hits `ENOSPC` mid-link is worse than one that asks for a little too
//! much. #1750 item 1 tightens it once per-stage peak footprints are recorded.

use std::path::Path;

use fbuild_build_engine::disk_preflight::{DiskRoot, FreeSpace, HostFreeSpace, preflight};
use fbuild_core::Result;

/// Free bytes the cache volume must have before a cold ESP32 build: a platform
/// package (~1.3 GB), a toolchain (~1.0 GB) and the framework install land
/// there. Already-cached packages shrink the real delta, but a host with less
/// than this cannot finish a cold build (FastLED/fbuild#1750).
pub const ESP32_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Free bytes the project volume must have: the per-env build tree plus the
/// firmware image and map file.
pub const PROJECT_BUILD_BYTES: u64 = 256 * 1024 * 1024;

/// Free bytes the temp volume must have for response files and tool scratch.
pub const TEMP_BYTES: u64 = 64 * 1024 * 1024;

/// Overrides [`ESP32_CACHE_BYTES`]. `0` disables the pre-flight check.
///
/// Read inside the daemon, which is where builds run, so the value must be in
/// the environment that spawned it — same convention as `FBUILD_PERF_LOG`
/// (`fbuild build` reuses an already-running daemon and does not forward env).
pub const MIN_DISK_ENV: &str = "FBUILD_MIN_DISK_BYTES";

/// The cache requirement, honoring [`MIN_DISK_ENV`].
pub fn cache_requirement() -> u64 {
    cache_requirement_from(std::env::var(MIN_DISK_ENV).ok().as_deref())
}

/// Pure form of [`cache_requirement`]: unset or unparseable keeps the default,
/// `0` disables the check, anything else is the required byte count.
pub fn cache_requirement_from(raw: Option<&str>) -> u64 {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return ESP32_CACHE_BYTES;
    };
    match raw.parse::<u64>() {
        Ok(bytes) => bytes,
        Err(_) => {
            tracing::warn!(
                value = raw,
                "{} is not a byte count; using the default ESP32 disk requirement",
                MIN_DISK_ENV
            );
            ESP32_CACHE_BYTES
        }
    }
}

/// Every root an ESP32 build writes, with its requirement.
///
/// Roots that live on the same volume are each checked against their own
/// requirement rather than a summed one: the numbers are conservative
/// per-root estimates, and a shared volume that satisfies every root
/// individually satisfies the build.
///
/// A cache requirement of `0` disables the whole check (the escape hatch for a
/// host whose `statvfs` reports something the estimate cannot anticipate), so
/// no root is checked at all in that case.
pub fn disk_roots(project_dir: &Path, cache_bytes: u64) -> Vec<DiskRoot> {
    if cache_bytes == 0 {
        return Vec::new();
    }
    vec![
        DiskRoot::new("cache", fbuild_paths::get_cache_root(), cache_bytes),
        DiskRoot::new("project", project_dir, PROJECT_BUILD_BYTES),
        DiskRoot::new("temp", std::env::temp_dir(), TEMP_BYTES),
    ]
}

/// Whether this build should be checked at all.
///
/// A build that only frees space (`--clean`, `--clean-all`, `fbuild clean`)
/// must never be blocked by a full disk: it is the remedy, not the consumer.
pub fn should_preflight(clean_only: bool, clean_all: bool) -> bool {
    !(clean_only || clean_all)
}

/// Run the pre-flight check for this project before package resolution.
pub fn preflight_esp32_disk(project_dir: &Path) -> Result<()> {
    let roots = disk_roots(project_dir, cache_requirement());
    if roots.is_empty() {
        tracing::debug!("disk preflight disabled by {}=0", MIN_DISK_ENV);
        return Ok(());
    }
    preflight(
        &HostFreeSpace as &dyn FreeSpace,
        &roots,
        &fbuild_paths::get_cache_root(),
    )
    .map(|_report| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_blank_keeps_the_default() {
        assert_eq!(cache_requirement_from(None), ESP32_CACHE_BYTES);
        assert_eq!(cache_requirement_from(Some("")), ESP32_CACHE_BYTES);
        assert_eq!(cache_requirement_from(Some("   ")), ESP32_CACHE_BYTES);
    }

    #[test]
    fn zero_disables_the_check_entirely() {
        assert_eq!(cache_requirement_from(Some("0")), 0);
        assert!(disk_roots(Path::new("/project"), 0).is_empty());
    }

    #[test]
    fn explicit_byte_counts_are_honored() {
        assert_eq!(cache_requirement_from(Some(" 1048576 ")), 1024 * 1024);
        let roots = disk_roots(Path::new("/project"), 1024 * 1024);
        assert_eq!(roots.len(), 3);
        assert_eq!(roots[0].label(), "cache");
        assert_eq!(roots[0].required_bytes(), 1024 * 1024);
        assert_eq!(roots[1].label(), "project");
        assert_eq!(roots[1].required_bytes(), PROJECT_BUILD_BYTES);
        assert_eq!(roots[2].label(), "temp");
        assert_eq!(roots[2].required_bytes(), TEMP_BYTES);
    }

    #[test]
    fn space_freeing_builds_are_never_blocked() {
        assert!(should_preflight(false, false));
        assert!(
            !should_preflight(true, false),
            "a clean-only build frees space"
        );
        assert!(
            !should_preflight(false, true),
            "a clean-all build frees space"
        );
        assert!(!should_preflight(true, true));
    }

    #[test]
    fn unparseable_values_fall_back_to_the_default() {
        assert_eq!(cache_requirement_from(Some("2GB")), ESP32_CACHE_BYTES);
        assert_eq!(cache_requirement_from(Some("-1")), ESP32_CACHE_BYTES);
    }
}
