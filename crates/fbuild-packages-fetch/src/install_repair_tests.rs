//! A sentinel-less install directory must be repaired, not trusted.
//!
//! `is_cached()` has always documented that the `.install_complete` sentinel is
//! what makes a directory an install, but `staged_install` used to return any
//! existing directory — so a partial tree (an interrupted extract, a CI cache
//! restored before the atomic rename, or a test that wrote into the real cache)
//! was handed back as a framework root forever (FastLED/fbuild#2043).

use std::path::Path;

use fbuild_core::path::NormalizedPath;

use crate::PackageBase;
use crate::chip_slice_tests::{base_pinned, serve, tar_zst, validate_gcc};
use crate::disk_cache;

/// The deterministic quarantine sibling of an install path.
fn quarantine_of(install: &Path) -> NormalizedPath {
    NormalizedPath::new(install.with_file_name(format!(
        "{}.incomplete",
        install.file_name().unwrap_or_default().to_string_lossy()
    )))
}

#[tokio::test]
async fn a_sentinel_less_install_dir_is_reinstalled_and_moved_aside() {
    let payload = tar_zst(&[("bin/xtensa-esp-elf-gcc", b"#!/bin/sh\n")]);
    let (url, hits) = serve(vec![("/vendor.tar.zst", payload)]).await;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("cache");
    let base = base_pinned(
        tmp.path(),
        &cache_root,
        &format!("{url}/vendor.tar.zst"),
        None,
    );
    let install = base.install_path();

    // Exactly the tree from the bug report: one variant file, no `bin/`, no
    // `.install_complete`, no index row.
    let stray = install.join("variants").join("fbuild-test-rpipico");
    std::fs::create_dir_all(&stray).expect("stray tree");
    std::fs::write(stray.join("memmap_default.ld"), b"FLASH=1").expect("stray script");
    assert!(
        !base.is_cached(),
        "a directory without the sentinel is not an install"
    );

    let installed = base
        .staged_install(validate_gcc)
        .await
        .expect("an incomplete install must be repaired, not returned");

    assert_eq!(installed, install, "the repair installs at the same path");
    assert!(
        disk_cache::paths::install_complete_sentinel(&install).exists(),
        "the repaired install must carry the sentinel"
    );
    assert!(
        install.join("bin").join("xtensa-esp-elf-gcc").is_file(),
        "the archive must actually have been extracted"
    );
    assert!(
        !stray.exists(),
        "the untrusted tree must not survive next to a fresh install"
    );
    assert_eq!(
        hits.lock().expect("hits").clone(),
        vec!["/vendor.tar.zst".to_string()],
        "a sentinel-less directory is not a cache hit: the archive is re-downloaded"
    );

    // Never deleted: the old tree is quarantined beside the install.
    let aside = quarantine_of(&install);
    assert!(
        aside
            .join("variants")
            .join("fbuild-test-rpipico")
            .join("memmap_default.ld")
            .is_file(),
        "an incomplete tree is moved aside for diagnosis, not destroyed ({})",
        aside.display()
    );
    assert!(base.is_cached(), "the repaired install is now cached");
}

#[tokio::test]
async fn a_repeat_repair_replaces_the_quarantine_instead_of_accumulating() {
    let payload = tar_zst(&[("bin/xtensa-esp-elf-gcc", b"#!/bin/sh\n")]);
    let (url, _hits) = serve(vec![("/vendor.tar.zst", payload)]).await;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("cache");
    let base = base_pinned(
        tmp.path(),
        &cache_root,
        &format!("{url}/vendor.tar.zst"),
        None,
    );
    let install = base.install_path();
    let aside = quarantine_of(&install);

    for marker in ["first", "second"] {
        std::fs::create_dir_all(&install).expect("install dir");
        std::fs::write(install.join("marker"), marker).expect("marker");
        // Losing the sentinel is what makes the tree untrustworthy; the second
        // iteration starts from the first repair's now-sentinel-less install.
        let sentinel = disk_cache::paths::install_complete_sentinel(&install);
        let _ = std::fs::remove_file(sentinel);
        if marker == "second" {
            // A stale quarantine from the previous repair is already there.
            std::fs::create_dir_all(&aside).expect("aside dir");
            std::fs::write(aside.join("stale"), b"old").expect("stale");
        }
        base.staged_install(validate_gcc).await.expect("repair");
    }

    assert_eq!(
        std::fs::read_to_string(aside.join("marker")).expect("quarantined marker"),
        "second",
        "the quarantine holds the tree from the latest repair"
    );
    assert!(
        !aside.join("stale").exists(),
        "the successor name is deterministic, so repairs do not accumulate trees"
    );
    let siblings = std::fs::read_dir(install.parent().expect("parent"))
        .expect("read cache dir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".incomplete"))
        .count();
    assert_eq!(siblings, 1, "exactly one quarantine sibling");
}

#[tokio::test]
async fn a_complete_install_is_still_a_hit_with_no_download() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("cache");
    let base = base_pinned(
        tmp.path(),
        &cache_root,
        "http://127.0.0.1:9/unused.tar.zst",
        None,
    );
    let install = base.install_path();
    std::fs::create_dir_all(install.join("bin")).expect("install");
    std::fs::write(install.join("bin").join("xtensa-esp-elf-gcc"), b"").expect("gcc");
    std::fs::write(disk_cache::paths::install_complete_sentinel(&install), b"").expect("sentinel");

    assert_eq!(
        base.complete_install_dir().expect("hit").as_path(),
        install.as_path()
    );
    assert_eq!(
        base.staged_install(validate_gcc).await.expect("warm hit"),
        install,
        "a complete install must keep short-circuiting (no network on this port)"
    );
    assert!(
        !quarantine_of(&install).exists(),
        "a complete install is never quarantined"
    );
}

#[test]
fn complete_install_dir_ignores_sentinel_less_dirs() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("cache");
    let base: PackageBase = base_pinned(tmp.path(), &cache_root, "http://127.0.0.1:9/x", None);
    let install = base.install_path();
    std::fs::create_dir_all(&install).expect("dir");
    assert!(
        base.complete_install_dir().is_none(),
        "existence alone is not a hit"
    );
    std::fs::write(disk_cache::paths::install_complete_sentinel(&install), b"").expect("sentinel");
    assert!(base.complete_install_dir().is_some());
}
