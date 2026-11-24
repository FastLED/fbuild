//! Tests for the canonical prebuilt-core fingerprint (`prebuilt_core.rs`) -
//! decision 8, FastLED/fbuild#1882.
//!
//! Kept in a sibling module so `tests.rs` stays under the 1000-LOC gate.

use std::path::Path;

use super::Esp32Framework;
use super::prebuilt_core::{CompileUnit, ConfigDigest, CoreBuildInputs, canonical_argv_digest};
use crate::{CacheSubdir, PackageBase};

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
        std::fs::create_dir_all(mcu_dir.join("include/freertos/FreeRTOS-Kernel/include/freertos"))
            .unwrap();
        std::fs::write(
            mcu_dir.join("include/freertos/FreeRTOS-Kernel/include/freertos/FreeRTOS.h"),
            "//freertos\n",
        )
        .unwrap();
        std::fs::create_dir_all(mcu_dir.join("flags")).unwrap();
        std::fs::write(mcu_dir.join("flags/includes"), "-Iinclude\n").unwrap();
        std::fs::create_dir_all(mcu_dir.join("lib")).unwrap();
        std::fs::write(mcu_dir.join("lib/libfreertos.a"), "//lib\n").unwrap();
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

fn tokens(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_string()).collect()
}

/// The compiled units of a two-TU core. Leaked so a test can pass the same
/// `&'static` slice to several plans; the process exits immediately after.
fn units() -> &'static [CompileUnit<'static>] {
    Box::leak(
        vec![
            CompileUnit {
                unit: "cores/esp32/esp32-hal-gpio.c",
                argv: Box::leak(
                    tokens(&[
                        "/toolchain/bin/xtensa-esp32-elf-gcc",
                        "-c",
                        "-o",
                        "obj/gpio.o",
                        "cores/esp32/esp32-hal-gpio.c",
                    ])
                    .into_boxed_slice(),
                ),
            },
            CompileUnit {
                unit: "cores/esp32/main.cpp",
                argv: Box::leak(
                    tokens(&[
                        "/toolchain/bin/xtensa-esp32-elf-g++",
                        "-c",
                        "-o",
                        "obj/main.o",
                        "cores/esp32/main.cpp",
                    ])
                    .into_boxed_slice(),
                ),
            },
        ]
        .into_boxed_slice(),
    )
}

fn config() -> &'static [ConfigDigest<'static>] {
    Box::leak(
        vec![
            ConfigDigest {
                id: "tools/esp32-arduino-libs/esp32c3/include/sdkconfig.h",
                sha256: "aa11",
            },
            ConfigDigest {
                id: "cores/esp32/esp32-hal.h",
                sha256: "bb22",
            },
        ]
        .into_boxed_slice(),
    )
}

fn inputs() -> CoreBuildInputs<'static> {
    CoreBuildInputs {
        compiler_abi: "xtensa-esp32-elf-gcc 14.2.0_20260121",
        toolchain_name: "toolchain-xtensa-esp32",
        toolchain_version: "14.2.0",
        mcu: "esp32c3",
        sdk_variant: "esp32c3",
        units: units(),
        config: config(),
    }
}

fn fingerprint(root: &Path, inputs: &CoreBuildInputs<'_>) -> Option<String> {
    framework_with(root)
        .prebuilt_core_plan(inputs)
        .map(|plan| plan.fingerprint().to_string())
}

#[test]
fn a_stamped_framework_plans_and_is_stable() {
    let tmp = tempfile::TempDir::new().unwrap();
    stamped_framework(tmp.path(), &["esp32c3"]);
    let plan = framework_with(tmp.path())
        .prebuilt_core_plan(&inputs())
        .expect("a stamped framework with canonical inputs must plan");
    assert_eq!(plan.fingerprint().len(), 64);
    assert_eq!(plan.mcu(), "esp32c3");
    assert_eq!(plan.sdk_variant(), "esp32c3");
    assert_eq!(plan.core_version(), "3.3.12");
    assert_eq!(
        Some(plan),
        framework_with(tmp.path()).prebuilt_core_plan(&inputs())
    );
}

#[test]
fn an_unstamped_framework_never_plans() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("cores/esp32")).unwrap();
    std::fs::write(tmp.path().join("cores/esp32/Arduino.h"), "#pragma once\n").unwrap();
    // A legacy tree: complete on disk, no identity anywhere.
    assert_eq!(fingerprint(tmp.path(), &inputs()), None);
}

#[test]
fn the_plan_is_install_history_independent() {
    let tmp = tempfile::TempDir::new().unwrap();
    stamped_framework(tmp.path(), &["esp32c3"]);
    let before = fingerprint(tmp.path(), &inputs()).expect("identity");

    // Building another chip into the same core tree adds that chip's component.
    stamped_framework(tmp.path(), &["esp32dev"]);

    assert_eq!(
        fingerprint(tmp.path(), &inputs()),
        Some(before),
        "another chip's component must not move this chip's fingerprint"
    );
}

#[test]
fn the_plan_does_not_depend_on_the_install_path() {
    let first = tempfile::TempDir::new().unwrap();
    let second = tempfile::TempDir::new().unwrap();
    stamped_framework(first.path(), &["esp32c3"]);
    stamped_framework(second.path(), &["esp32c3"]);
    let plan = fingerprint(first.path(), &inputs());
    assert!(plan.is_some());
    assert_eq!(plan, fingerprint(second.path(), &inputs()));
}

#[test]
fn a_different_invocation_yields_a_different_fingerprint() {
    let tmp = tempfile::TempDir::new().unwrap();
    stamped_framework(tmp.path(), &["esp32c3"]);
    let base = fingerprint(tmp.path(), &inputs()).expect("identity");

    // One extra -D changes the compiled bytes, so it must move the key.
    let mut changed_inputs = inputs();
    changed_inputs.units = Box::leak(
        vec![
            CompileUnit {
                unit: "cores/esp32/esp32-hal-gpio.c",
                argv: units()[0].argv,
            },
            CompileUnit {
                unit: "cores/esp32/main.cpp",
                argv: Box::leak(
                    tokens(&[
                        "/toolchain/bin/xtensa-esp32-elf-g++",
                        "-c",
                        "-DEXTRA=1",
                        "-o",
                        "obj/main.o",
                        "cores/esp32/main.cpp",
                    ])
                    .into_boxed_slice(),
                ),
            },
        ]
        .into_boxed_slice(),
    );
    assert_ne!(
        base,
        fingerprint(tmp.path(), &changed_inputs).expect("identity")
    );

    // A different config/header digest moves it too.
    let mut other_config = inputs();
    other_config.config = Box::leak(
        vec![ConfigDigest {
            id: "cores/esp32/esp32-hal.h",
            sha256: "cc33",
        }]
        .into_boxed_slice(),
    );
    assert_ne!(
        base,
        fingerprint(tmp.path(), &other_config).expect("identity")
    );

    // A different toolchain version is a different invocation as well.
    let mut other_toolchain = inputs();
    other_toolchain.toolchain_version = "13.2.0";
    assert_ne!(
        base,
        fingerprint(tmp.path(), &other_toolchain).expect("identity")
    );
}

#[test]
fn the_unit_list_order_is_canonical() {
    let forward = units();
    let mut backward = forward.to_vec();
    backward.reverse();

    // Same units, enumerated in another order: one plan.
    assert_eq!(
        canonical_argv_digest(forward),
        canonical_argv_digest(&backward)
    );

    // A changed argument order inside a unit is a different invocation.
    let mut reordered = forward.to_vec();
    let mut words = reordered[0].argv.to_vec();
    words.swap(1, 2);
    reordered[0].argv = Box::leak(words.into_boxed_slice());
    assert_ne!(
        canonical_argv_digest(forward),
        canonical_argv_digest(&reordered)
    );
}

#[test]
fn incomplete_inputs_never_plan() {
    let tmp = tempfile::TempDir::new().unwrap();
    stamped_framework(tmp.path(), &["esp32c3"]);

    // No units -> no object to key.
    let mut no_units = inputs();
    no_units.units = &[];
    assert_eq!(fingerprint(tmp.path(), &no_units), None);

    // No config digests -> the header state is unknown.
    let mut no_config = inputs();
    no_config.config = &[];
    assert_eq!(fingerprint(tmp.path(), &no_config), None);

    // An empty toolchain identity -> the bytes are not attributable.
    let mut no_toolchain = inputs();
    no_toolchain.compiler_abi = " ";
    assert_eq!(fingerprint(tmp.path(), &no_toolchain), None);

    // A variant the tree does not carry -> no identity, so no plan.
    let mut other_variant = inputs();
    other_variant.sdk_variant = "esp32s3";
    assert_eq!(fingerprint(tmp.path(), &other_variant), None);
}
