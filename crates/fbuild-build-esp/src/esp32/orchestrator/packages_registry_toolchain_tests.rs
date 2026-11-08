//! Registry/manifest toolchain-name selection (moved out of `packages.rs`,
//! which sits at the 1000-LOC ceiling).
//!
//! These run against the legacy and pioarduino `platform.json` shapes: which
//! package name a manifest declares, and which one an MCU-primary toolchain
//! resolves to. `packages.rs` calls the same helpers on the build path.
use super::*;
use fbuild_core::platformio_package::{PackageKind as RegistryKind, resolve_registry_json};

#[test]
fn legacy_esp32s3_manifest_selects_its_per_mcu_toolchain() {
    let mcu = get_mcu_config("esp32s3").unwrap();
    assert_eq!(
        per_mcu_toolchain_name(&mcu).as_deref(),
        Some("toolchain-xtensa-esp32s3")
    );
    assert_eq!(
        primary_toolchain_name(mcu.is_riscv()),
        "toolchain-xtensa-esp-elf"
    );
    assert_eq!(mcu.toolchain_prefix(), "xtensa-esp32s3-elf-");
    assert_eq!(
        per_mcu_toolchain_name(&get_mcu_config("esp32c3").unwrap()),
        None
    );
}

#[test]
fn declared_per_mcu_toolchain_wins_over_unified() {
    let s3 = get_mcu_config("esp32s3").unwrap();
    let c3 = get_mcu_config("esp32c3").unwrap();
    // official espressif32 6.x/7.x: both declared, Arduino uses per-MCU.
    let both = |n: &str| n == "toolchain-xtensa-esp32s3" || n == "toolchain-xtensa-esp-elf";
    assert_eq!(toolchain_name_for(&s3, both), "toolchain-xtensa-esp32s3");
    // pioarduino 51.x: per-MCU only.
    assert_eq!(
        toolchain_name_for(&s3, |n| n == "toolchain-xtensa-esp32s3"),
        "toolchain-xtensa-esp32s3"
    );
    // pioarduino 53.x/54.x: unified only, by registry version.
    assert_eq!(
        toolchain_name_for(&s3, |n| n == "toolchain-xtensa-esp-elf"),
        "toolchain-xtensa-esp-elf"
    );
    // RISC-V always uses the shared package.
    assert_eq!(toolchain_name_for(&c3, both), "toolchain-riscv32-esp");
}

#[test]
fn registry_payload_becomes_exact_checked_toolchain_package() {
    let manifest = r#"{"packages":{"toolchain-xtensa-esp32s3":{"type":"toolchain","owner":"espressif","version":"12.2.0+20230208"}}}"#;
    let requirement = fbuild_core::platformio_package::resolve_platform_requirements(manifest, &[])
        .unwrap()
        .remove(0);
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let response = format!(
        "{{\"name\":\"toolchain-xtensa-esp32s3\",\"owner\":{{\"username\":\"espressif\"}},\"versions\":[{{\"name\":\"12.2.0+20230208\",\"files\":[{{\"system\":[\"linux_x86_64\"],\"download_url\":\"https://example.test/s3-gcc12.tar.gz\",\"checksum\":{{\"sha256\":\"{hash}\"}}}}]}}]}}"
    );
    let payload = resolve_registry_json(
        requirement.spec.registry().unwrap(),
        RegistryKind::Tool,
        "linux_x86_64",
        &response,
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let toolchain = fbuild_packages::toolchain::Esp32Toolchain::from_registry_payload(
        temp.path(),
        &payload,
        "xtensa-esp32s3-elf-",
    );
    let info = fbuild_packages::Package::get_info(&toolchain);
    assert_eq!(info.name, "toolchain-xtensa-esp32s3");
    assert_eq!(info.version, "12.2.0+20230208");
    assert_eq!(info.url, "https://example.test/s3-gcc12.tar.gz");
    assert_eq!(info.checksum.as_deref(), Some(hash));

    cache_registry_payload(
        temp.path(),
        requirement.spec.registry().unwrap(),
        "linux_x86_64",
        &payload,
    )
    .unwrap();
    assert_eq!(
        cached_registry_payload(
            temp.path(),
            requirement.spec.registry().unwrap(),
            "linux_x86_64"
        )
        .unwrap(),
        Some(payload)
    );
    assert!(
        cached_registry_payload(
            temp.path(),
            requirement.spec.registry().unwrap(),
            "darwin_arm64"
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn legacy_xtensa_and_riscv_registry_files_select_each_host() {
    let digest = "b".repeat(64);
    for (name, prefix) in [
        ("toolchain-xtensa-esp32s3", "xtensa-esp32s3-elf-"),
        ("toolchain-riscv32-esp", "riscv32-esp-elf-"),
    ] {
        let manifest = serde_json::json!({"packages": {
            name: {"type":"toolchain", "owner":"espressif", "version":"12.2.0+20230208"}
        }});
        let requirement = fbuild_core::platformio_package::resolve_platform_requirements(
            &manifest.to_string(),
            &[],
        )
        .unwrap()
        .remove(0);
        let response = serde_json::json!({
            "name": name,
            "owner": {"username":"espressif"},
            "versions": [{"name":"12.2.0+20230208", "files": [
                {"system":["linux_x86_64"], "download_url":format!("https://example.test/{name}-linux.tar.gz"), "checksum":{"sha256":digest}},
                {"system":["windows_amd64","windows_arm64"], "download_url":format!("https://example.test/{name}-windows.tar.gz"), "checksum":{"sha256":digest}},
                {"system":["darwin_arm64"], "download_url":format!("https://example.test/{name}-macos.tar.gz"), "checksum":{"sha256":digest}}
            ]}]
        });
        for (system, suffix) in [
            ("linux_x86_64", "linux"),
            ("windows_amd64", "windows"),
            ("darwin_arm64", "macos"),
        ] {
            let payload = resolve_registry_json(
                requirement.spec.registry().unwrap(),
                RegistryKind::Tool,
                system,
                &response.to_string(),
            )
            .unwrap();
            assert_eq!(
                payload.url,
                format!("https://example.test/{name}-{suffix}.tar.gz")
            );
            assert_eq!(payload.sha256, digest);
            let temp = tempfile::tempdir().unwrap();
            let package = fbuild_packages::toolchain::Esp32Toolchain::from_registry_payload(
                temp.path(),
                &payload,
                prefix,
            );
            assert_eq!(
                fbuild_packages::Package::get_info(&package).version,
                "12.2.0+20230208"
            );
        }
    }
}

/// The registry payload is the toolchain package on every platform that
/// declares a registry version instead of a metadata URL -- including the
/// bundled `espressif32` (FastLED/fbuild#1961). It must carry the MCU's chip,
/// or `published_slice` returns before it ever consults the mirror manifest
/// and the whole 168 MB archive is downloaded for a chip whose slice is
/// published (the 2026-11-08 repro downloaded `esp32-xtensa-gcc`'s whole
/// archive and created no `+esp32s3` install dir).
#[test]
fn registry_toolchain_scopes_the_chip_so_a_published_slice_can_be_selected() {
    let digest = "c".repeat(64);
    let manifest = r#"{"packages":{"toolchain-xtensa-esp-elf":{"type":"toolchain","owner":"platformio","version":"14.2.0+r1"}}}"#;
    let requirement = fbuild_core::platformio_package::resolve_platform_requirements(manifest, &[])
        .unwrap()
        .remove(0);
    let response = format!(
        "{{\"name\":\"toolchain-xtensa-esp-elf\",\"owner\":{{\"username\":\"platformio\"}},\"versions\":[{{\"name\":\"14.2.0+r1\",\"files\":[{{\"system\":[\"linux_x86_64\"],\"download_url\":\"https://example.test/xtensa-esp-elf-linux-amd64.tar.gz\",\"checksum\":{{\"sha256\":\"{digest}\"}}}}]}}]}}"
    );
    let payload = resolve_registry_json(
        requirement.spec.registry().unwrap(),
        RegistryKind::Tool,
        "linux_x86_64",
        &response,
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();

    let s3 = get_mcu_config("esp32s3").unwrap();
    let toolchain = registry_toolchain(temp.path(), &payload, &s3.toolchain_prefix(), &s3);
    let chip_path = toolchain
        .chip_install_path()
        .expect("the registry toolchain must carry its MCU's chip");
    let chip_path = chip_path.as_path();
    assert!(
        chip_path.to_string_lossy().ends_with("+esp32s3"),
        "chip install path was {}",
        chip_path.display()
    );

    // RISC-V multilibs are ISA-shaped: never sliced, so it keeps the shared
    // directory every RISC-V MCU can use.
    let c3 = get_mcu_config("esp32c3").unwrap();
    assert!(
        registry_toolchain(temp.path(), &payload, &c3.toolchain_prefix(), &c3)
            .chip_install_path()
            .is_none()
    );
}
