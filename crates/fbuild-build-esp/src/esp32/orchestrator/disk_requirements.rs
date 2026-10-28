//! Disk-space requirements the ESP32 build hands to the engine pre-flight check
//! (FastLED/fbuild#1750).
//!
//! The check itself lives in `fbuild_build_engine::disk_preflight`; this module
//! owns the ESP32 numbers, because only this crate knows which roots an ESP32
//! build writes and how much room a cold one needs. The estimate is
//! deliberately conservative and rounded up: a pre-flight check that passes and
//! then hits `ENOSPC` mid-link is worse than one that asks for a little too
//! much. The numbers below are now measured rather than estimated: `du` over
//! hydrated installs, recorded per component in each constant's documentation
//! (#1750 item 1). `disk_footprint` records what a build actually consumes so
//! the next correction comes from data, not from a guess.

use std::path::Path;

use fbuild_build_engine::disk_footprint::FootprintRoot;
use fbuild_build_engine::disk_preflight::{DiskRoot, FreeSpace, HostFreeSpace, preflight};
use fbuild_core::Result;
use fbuild_core::path::NormalizedPath;

/// Free bytes the cache volume must have before a cold ESP32 build: a platform
/// package, a toolchain and the framework install all land there, so this is
/// the measured cold-worst-case footprint plus a small margin
/// (FastLED/fbuild#1750 item 1).
///
/// Measured on a hydrated Linux host (2026-10-28), by `du` over installed
/// packages rather than estimated:
///
/// | component                                   | measured |
/// |---------------------------------------------|----------|
/// | pioarduino framework install (incl. `tools/esp32-arduino-libs`, 2.7 GB) | 2.2–2.8 GB |
/// | platform package (`pioarduino-platform-espressif32`)                  | 28 MB    |
/// | Xtensa toolchain (`toolchain-xtensa-esp-elf`)                         | 1.1 GB   |
/// | RISC-V toolchain (`toolchain-riscv32-esp`, ESP32-C3/C6/H2/P4)         | 2.1 GB   |
///
/// A cold Xtensa build therefore needs ~3.9 GiB and a cold RISC-V build
/// ~4.9 GiB of cache volume; 5 GiB covers both. A host that already holds part
/// of this needs less, and [`MIN_DISK_ENV`] lowers the requirement outright —
/// the check must not ask for room the build will not use.
///
/// The previous estimate (2 GiB) was less than a framework install alone, so it
/// passed hosts that then hit `ENOSPC` mid-link — the exact failure this check
/// exists to prevent.
pub const ESP32_CACHE_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Free bytes the project volume must have: the per-env build tree plus the
/// firmware image and map file.
///
/// Measured 2026-10-28 over nine ESP32 test projects on a Linux host: 15–59 MB
/// per build tree (the largest, ESP32-S3, at 59 MB), so this keeps a wide
/// margin above observation rather than being tightened to fit it
/// (FastLED/fbuild#1750 item 1).
pub const PROJECT_BUILD_BYTES: u64 = 256 * 1024 * 1024;

/// Free bytes fbuild's scratch tree must have for response files and tool
/// scratch. fbuild's scratch lives under the cache root (`<cache>/tmp`), not
/// `$TMPDIR`, per GATE-012's rooted-tempdir rule.
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
        DiskRoot::new("temp", scratch_root(), TEMP_BYTES),
    ]
}

/// The roots a footprint measurement watches (#1750 item 1): every root an
/// ESP32 build writes, with no requirement attached. This is measurement, not
/// judgement — a footprint log must never be able to block a build.
pub fn footprint_roots(project_dir: &Path) -> Vec<FootprintRoot> {
    vec![
        FootprintRoot::new("cache", fbuild_paths::get_cache_root()),
        FootprintRoot::new("project", project_dir),
        FootprintRoot::new("temp", scratch_root()),
    ]
}

/// fbuild's scratch tree: `<cache root>/tmp` (never `$TMPDIR`).
pub fn scratch_root() -> NormalizedPath {
    NormalizedPath::from(fbuild_paths::get_cache_root().join("tmp"))
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
    fn footprint_watches_the_same_three_roots_without_requirements() {
        let roots = footprint_roots(Path::new("/project"));
        let labels: Vec<&str> = roots.iter().map(FootprintRoot::label).collect();
        assert_eq!(labels, vec!["cache", "project", "temp"]);
        assert_eq!(roots[1].path(), Path::new("/project"));
        assert_eq!(roots[2].path(), scratch_root().as_path());
    }

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
