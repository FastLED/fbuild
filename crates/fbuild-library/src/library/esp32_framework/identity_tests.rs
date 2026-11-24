//! Tests for the framework identity term (`identity.rs`) - decision 8a,
//! FastLED/fbuild#1882.
//!
//! Kept in a sibling module so `tests.rs` stays under the 1000-LOC gate.

use std::path::Path;

use super::Esp32Framework;
use crate::{CacheSubdir, PackageBase};

fn complete_sdk_tree(dir: &Path) {
    std::fs::create_dir_all(dir.join("include/freertos/FreeRTOS-Kernel/include/freertos")).unwrap();
    std::fs::write(
        dir.join("include/freertos/FreeRTOS-Kernel/include/freertos/FreeRTOS.h"),
        "//freertos\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("flags")).unwrap();
    std::fs::write(dir.join("flags/includes"), "-Iinclude\n").unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/libfreertos.a"), "//lib\n").unwrap();
}

fn framework_with(root: &Path) -> Esp32Framework {
    Esp32Framework {
        base: PackageBase::new(
            "test",
            "3.3.12",
            "http://example.com",
            "http://example.com",
            None,
            CacheSubdir::Platforms,
            root,
        ),
        install_dir: Some(root.to_path_buf()),
    }
}

/// A stamped framework: core at the root, one stamped SDK variant per chip.
fn stamped_framework(root: &Path, chips: &[&str]) {
    use fbuild_packages_fetch::component_id::{ComponentIdentity, content_id, write_stamp};

    std::fs::create_dir_all(root.join("cores/esp32")).unwrap();
    std::fs::write(root.join("cores/esp32/Arduino.h"), "#pragma once\n").unwrap();
    write_stamp(
        root,
        &ComponentIdentity::new(
            "core",
            "3.3.12",
            content_id(root, &["tools/esp32-arduino-libs", "tools/.libs-archives"]).unwrap(),
            super::CORE_RECIPE,
        ),
    )
    .unwrap();
    for chip in chips {
        let mcu_dir = root.join("tools/esp32-arduino-libs").join(chip);
        complete_sdk_tree(&mcu_dir);
        write_stamp(
            &mcu_dir,
            &ComponentIdentity::new(
                &format!("sdk-{chip}"),
                "3.3.12",
                content_id(&mcu_dir, &[]).unwrap(),
                super::libs::SDK_RECIPE,
            ),
        )
        .unwrap();
    }
}

#[test]
fn framework_identity_covers_a_stamped_framework() {
    let tmp = tempfile::TempDir::new().unwrap();
    stamped_framework(tmp.path(), &["esp32c3"]);
    let fw = framework_with(tmp.path());
    let identity = fw.framework_identity("esp32c3", "esp32c3");
    assert!(identity.is_some());
    // Reading the same tree twice is stable: the term is derived from stamps,
    // never from a fresh tree walk.
    assert_eq!(identity, fw.framework_identity("esp32c3", "esp32c3"));
}

#[test]
fn framework_identity_is_install_history_independent() {
    let tmp = tempfile::TempDir::new().unwrap();
    stamped_framework(tmp.path(), &["esp32c3"]);
    let fw = framework_with(tmp.path());
    let before = fw
        .framework_identity("esp32c3", "esp32c3")
        .expect("identity");

    // Building another chip into the same core tree adds that chip's component.
    stamped_framework(tmp.path(), &["esp32dev"]);

    assert_eq!(
        fw.framework_identity("esp32c3", "esp32c3"),
        Some(before),
        "another chip's component must not move this chip's framework term"
    );
}

#[test]
fn framework_identity_of_an_unstamped_framework_is_none() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("cores/esp32")).unwrap();
    std::fs::write(root.join("cores/esp32/Arduino.h"), "#pragma once\n").unwrap();
    complete_sdk_tree(&root.join("tools/esp32-arduino-libs/esp32c3"));

    // A legacy tree: complete on disk, no identity anywhere.
    assert_eq!(
        framework_with(root).framework_identity("esp32c3", "esp32c3"),
        None
    );
}

#[test]
fn framework_identity_requires_every_sdk_variant_stamp() {
    let tmp = tempfile::TempDir::new().unwrap();
    stamped_framework(tmp.path(), &["esp32p4"]);
    let es = tmp.path().join("tools/esp32-arduino-libs/esp32p4_es");
    complete_sdk_tree(&es);

    // The chip-variant tree has no stamp of its own, so the board's framework
    // bytes are not fully identified.
    assert_eq!(
        framework_with(tmp.path()).framework_identity("esp32p4", "esp32p4_es"),
        None
    );
    // Without the extra variant the same tree is identified.
    assert!(
        framework_with(tmp.path())
            .framework_identity("esp32p4", "esp32p4")
            .is_some()
    );
}

#[test]
fn framework_identity_is_none_when_a_required_variant_tree_is_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    stamped_framework(tmp.path(), &["esp32c3"]);
    assert_eq!(
        framework_with(tmp.path()).framework_identity("esp32s3", "esp32s3"),
        None
    );
}

#[test]
fn framework_identity_includes_the_mcu_skeleton() {
    use fbuild_packages_fetch::component_id::{ComponentIdentity, content_id, write_stamp};

    let tmp = tempfile::TempDir::new().unwrap();
    stamped_framework(tmp.path(), &["esp32c2"]);
    let fw = framework_with(tmp.path());
    let without_skeleton = fw
        .framework_identity("esp32c2", "esp32c2")
        .expect("identity");

    let mcu_dir = tmp.path().join("tools/esp32-arduino-libs/esp32c2");
    write_stamp(
        &mcu_dir,
        &ComponentIdentity::new(
            "mcu-esp32c2",
            "3.3.12",
            content_id(&mcu_dir, &[]).unwrap(),
            super::libs::MCU_SKELETON_RECIPE,
        ),
    )
    .unwrap();

    let with_skeleton = fw
        .framework_identity("esp32c2", "esp32c2")
        .expect("identity");
    assert_ne!(
        without_skeleton, with_skeleton,
        "the skeleton's bytes are part of the framework"
    );
}
