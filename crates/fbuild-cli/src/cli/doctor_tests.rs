//! Tests for `fbuild doctor --check-disk` (FastLED/fbuild#1750 item 5).
//!
//! Every assertion runs against an injected volume, so the suite never depends
//! on this host's real free space.

use std::io;
use std::path::Path;

use fbuild_build::disk_preflight::{DiskRoot, FreeSpace, ReclaimableCache, measure};

use super::{project_dir_to_measure, render_disk_check};

const MB: u64 = 1024 * 1024;
const GB: u64 = 1024 * MB;

/// A volume that always reports the same free bytes.
struct FixedVolume(u64);

impl FreeSpace for FixedVolume {
    fn available(&self, _path: &Path) -> std::io::Result<u64> {
        Ok(self.0)
    }
}

/// A volume `statvfs` cannot read.
struct UnreadableVolume;

impl FreeSpace for UnreadableVolume {
    fn available(&self, _path: &Path) -> std::io::Result<u64> {
        Err(io::Error::other("statvfs failed"))
    }
}

fn roots() -> Vec<DiskRoot> {
    vec![
        DiskRoot::new("cache", Path::new("/cache/fbuild"), 2 * GB),
        DiskRoot::new("project", Path::new("/proj/build"), 256 * MB),
    ]
}

fn caches() -> Vec<ReclaimableCache> {
    vec![
        ReclaimableCache {
            name: "platforms".to_string(),
            bytes: 3 * GB,
        },
        ReclaimableCache {
            name: "zccache".to_string(),
            bytes: GB,
        },
    ]
}

fn render(volume: &dyn FreeSpace) -> super::DiskCheck {
    let report = measure(volume, &roots());
    render_disk_check(&report, &caches(), Path::new("/cache/fbuild"), false)
}

#[test]
fn a_roomy_host_reports_ok() {
    let check = render(&FixedVolume(41 * GB));
    assert!(check.ok, "41 GB is plenty: {}", check.text);
    assert!(check.text.contains("/cache/fbuild"), "{}", check.text);
    assert!(check.text.contains("2.00GB required"), "{}", check.text);
    assert!(check.text.contains("verdict: ok"), "{}", check.text);
    assert!(
        !check.text.contains("SHORT"),
        "nothing is short: {}",
        check.text
    );
}

#[test]
fn a_short_root_names_the_shortfall_and_fails() {
    let check = render(&FixedVolume(512 * MB));
    assert!(!check.ok, "512 MB cannot host a cold build: {}", check.text);
    // cache wants 2 GB, has 512 MB -> short by 1.50GB.
    assert!(check.text.contains("SHORT by 1.50GB"), "{}", check.text);
    assert!(check.text.contains("/cache/fbuild"), "{}", check.text);
    assert!(check.text.contains("verdict: FAIL"), "{}", check.text);
    // The actionable block a build would print is reused verbatim.
    assert!(
        check.text.contains("insufficient disk space:"),
        "{}",
        check.text
    );
}

#[test]
fn only_the_short_root_is_flagged() {
    // 300 MB satisfies the project root (256 MB) but not the cache root (2 GB).
    let check = render(&FixedVolume(300 * MB));
    assert!(!check.ok, "the cache root is short: {}", check.text);
    let short_lines = check
        .text
        .lines()
        .filter(|l| l.contains("SHORT by"))
        .count();
    assert_eq!(short_lines, 1, "only the cache root: {}", check.text);
}

#[test]
fn an_unmeasurable_volume_is_a_warning_not_a_failure() {
    let check = render(&UnreadableVolume);
    assert!(
        check.ok,
        "an unreadable volume must not fail: {}",
        check.text
    );
    assert!(
        check.text.contains("could not be measured"),
        "{}",
        check.text
    );
    assert!(
        check.text.contains("not counted as a failure"),
        "{}",
        check.text
    );
    assert!(
        check.text.contains("verdict: unknown"),
        "no root was measured: {}",
        check.text
    );
}

#[test]
fn reclaimable_caches_are_listed_with_a_total() {
    let check = render(&FixedVolume(41 * GB));
    let platforms = check.text.find("platforms").expect("platforms listed");
    let zccache = check.text.find("zccache").expect("zccache listed");
    assert!(platforms < zccache, "largest first: {}", check.text);
    assert!(
        check.text.contains("4.00GB"),
        "total of 4 GB: {}",
        check.text
    );
}

#[test]
fn an_empty_cache_root_says_none() {
    let report = measure(&FixedVolume(41 * GB), &roots());
    let check = render_disk_check(&report, &[], Path::new("/cache/fbuild"), false);
    assert!(check.ok, "{}", check.text);
    assert!(check.text.contains("  none"), "{}", check.text);
}

#[test]
fn a_disabled_check_is_skipped_not_failed() {
    let report = measure(&FixedVolume(0), &[]);
    let check = render_disk_check(&report, &[], Path::new("/cache/fbuild"), true);
    assert!(check.ok, "disabled must not fail: {}", check.text);
    assert!(
        check.text.contains("FBUILD_MIN_DISK_BYTES=0"),
        "{}",
        check.text
    );
    assert!(check.text.contains("skipped"), "{}", check.text);
}

#[test]
fn the_project_dir_is_made_absolute_without_touching_the_filesystem() {
    let resolved = project_dir_to_measure(Some("some/missing/dir"));
    assert!(
        resolved.as_path().is_absolute(),
        "doctor must send an absolute path: {}",
        resolved.display()
    );
    assert!(
        resolved.ends_with(Path::new("some/missing/dir")),
        "{}",
        resolved.display()
    );
    // Default is the current directory.
    assert!(project_dir_to_measure(None).as_path().is_absolute());
}
