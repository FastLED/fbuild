//! Issue #1491: PlatformIO package specs must resolve independently of native
//! platform dispatch. These fixtures are offline and intentionally include a
//! platform name that fbuild cannot build.

#[cfg(windows)]
use fbuild_core::path::NormalizedPath;
use fbuild_core::platformio_package::{
    PackageKind, PackageLock, PackageSource, ResolutionError, parse_package_spec, registry_api_url,
    require_platform_package, resolve_platform_requirements, resolve_registry_json,
    sole_optional_manifest_version,
};

#[test]
fn parses_generic_registry_aliases_without_platform_dispatch() {
    let short = parse_package_spec("espressif32@6.13.0").unwrap();
    let qualified = parse_package_spec("platformio/espressif32@^6.0.0").unwrap();
    let unknown = parse_package_spec("acme/custom-board@1.2.3").unwrap();

    assert_eq!(short.registry().unwrap().name, "espressif32");
    assert_eq!(
        short.registry().unwrap().requirement.as_deref(),
        Some("6.13.0")
    );
    assert_eq!(
        qualified.registry().unwrap().owner.as_deref(),
        Some("platformio")
    );
    assert_eq!(
        qualified.registry().unwrap().requirement.as_deref(),
        Some("^6.0.0")
    );
    assert_eq!(
        registry_api_url(PackageKind::Platform, unknown.registry().unwrap()).unwrap(),
        "https://api.registry.platformio.org/v3/packages/acme/platform/custom-board"
    );
}

#[test]
fn explicit_registry_type_paths_normalize_without_native_dispatch() {
    let owner_first =
        parse_package_spec("platformio/tool/toolchain-gccarmnoneeabi@1.90201.191206").unwrap();
    let type_first =
        parse_package_spec("tool/platformio/toolchain-gccarmnoneeabi@1.90201.191206").unwrap();
    let ordinary =
        parse_package_spec("platformio/toolchain-gccarmnoneeabi@1.90201.191206").unwrap();
    assert_eq!(owner_first.registry(), type_first.registry());
    assert_eq!(
        owner_first.registry().unwrap().name,
        ordinary.registry().unwrap().name
    );
    assert_eq!(
        registry_api_url(PackageKind::Tool, owner_first.registry().unwrap()).unwrap(),
        "https://api.registry.platformio.org/v3/packages/platformio/tool/toolchain-gccarmnoneeabi"
    );
    assert!(registry_api_url(PackageKind::Platform, owner_first.registry().unwrap()).is_err());
    assert!(parse_package_spec("platformio/unknown/toolchain-gccarmnoneeabi@1.0.0").is_err());
    let type_named_owner = parse_package_spec("tool/library/example@1.0.0").unwrap();
    assert_eq!(
        type_named_owner.registry().unwrap().owner.as_deref(),
        Some("tool")
    );
    assert_eq!(
        registry_api_url(PackageKind::Library, type_named_owner.registry().unwrap()).unwrap(),
        "https://api.registry.platformio.org/v3/packages/tool/library/example"
    );
}

#[test]
fn classifies_archive_repository_and_local_payload_paths() {
    let archive = parse_package_spec("https://example.test/platform.zip").unwrap();
    let named_archive =
        parse_package_spec("framework-arduinoespressif32@https://example.test/core.tar.gz")
            .unwrap();
    let git = parse_package_spec("https://github.com/acme/platform.git#deadbeef").unwrap();
    let bare_git = parse_package_spec("https://github.com/acme/platform").unwrap();
    let shorthand = parse_package_spec("acme/platform#v1.2.3").unwrap();
    let compressed_archive = parse_package_spec("https://example.test/platform.tar.zst").unwrap();
    let revision_archive =
        parse_package_spec("framework-foo@https://example.test/core.tar.gz#deadbeef").unwrap();
    let local = parse_package_spec("../platforms/custom").unwrap();
    let local_dir = tempfile::tempdir().unwrap();
    let local_url = reqwest::Url::from_file_path(local_dir.path()).unwrap();
    let file_url = parse_package_spec(local_url.as_str()).unwrap();

    assert!(matches!(archive.source, PackageSource::Archive { .. }));
    assert_eq!(
        named_archive.alias.as_deref(),
        Some("framework-arduinoespressif32")
    );
    assert!(matches!(git.source, PackageSource::Repository { .. }));
    assert!(matches!(bare_git.source, PackageSource::Repository { .. }));
    assert!(matches!(shorthand.source, PackageSource::Repository { .. }));
    assert!(matches!(
        compressed_archive.source,
        PackageSource::Archive { .. }
    ));
    assert!(matches!(
        revision_archive.source,
        PackageSource::Archive { url, revision: Some(revision) }
            if url == "https://example.test/core.tar.gz" && revision == "deadbeef"
    ));
    assert!(matches!(local.source, PackageSource::LocalPath { .. }));
    assert!(matches!(file_url.source, PackageSource::LocalPath { .. }));
    assert!(parse_package_spec("bad alias@not a version").is_err());
}

#[cfg(windows)]
#[test]
fn windows_file_url_maps_to_drive_path() {
    let spec = parse_package_spec("file:///C:/platforms/custom").unwrap();
    assert!(
        matches!(spec.source, PackageSource::LocalPath { path } if path == NormalizedPath::new(r"C:\platforms\custom"))
    );
}

#[test]
fn resolves_exact_platform_payload_and_checksum() {
    let spec = parse_package_spec("platformio/espressif32@6.13.0").unwrap();
    let response = r#"{
        "name":"espressif32", "owner":{"username":"platformio"},
        "versions":[{"name":"6.13.0","files":[{
            "system":"*", "download_url":"https://dl.registry.platformio.org/download/platformio/platform/espressif32/6.13.0/espressif32-6.13.0.tar.gz",
            "checksum":{"sha256":"5d1032b43828773ba87cf2e509432202c0bfe64f7304b58c9d669f13b116c6e0"}
        }]}]
    }"#;
    let payload = resolve_registry_json(
        spec.registry().unwrap(),
        PackageKind::Platform,
        "linux_x86_64",
        response,
    )
    .unwrap();

    assert_eq!(payload.version, "6.13.0");
    assert!(payload.url.ends_with("/espressif32-6.13.0.tar.gz"));
    assert_eq!(
        payload.sha256,
        "5d1032b43828773ba87cf2e509432202c0bfe64f7304b58c9d669f13b116c6e0"
    );
}

#[test]
fn selects_host_file_and_range_without_substituting_another_build() {
    let response = r#"{
        "name":"toolchain-xtensa-esp32s3", "owner":{"username":"espressif"},
        "versions":[
            {"name":"8.4.0+2021r2-patch4","files":[{"system":["linux_x86_64"],"download_url":"https://example.test/patch4.tar.gz","checksum":{"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}]},
            {"name":"8.4.0+2021r2-patch5","files":[
                {"system":["linux_x86_64"],"download_url":"https://example.test/linux.tar.gz","checksum":{"sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}},
                {"system":["windows_amd64","windows_arm64"],"download_url":"https://example.test/windows.tar.gz","checksum":{"sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}}
            ]}
        ]
    }"#;
    let exact =
        parse_package_spec("espressif/toolchain-xtensa-esp32s3@8.4.0+2021r2-patch5").unwrap();
    let payload = resolve_registry_json(
        exact.registry().unwrap(),
        PackageKind::Tool,
        "windows_arm64",
        response,
    )
    .unwrap();
    assert_eq!(payload.version, "8.4.0+2021r2-patch5");
    assert_eq!(payload.url, "https://example.test/windows.tar.gz");
    assert!(
        resolve_registry_json(
            exact.registry().unwrap(),
            PackageKind::Tool,
            "darwin_arm64",
            response
        )
        .is_err()
    );

    let range = parse_package_spec("espressif/toolchain-xtensa-esp32s3@>=8.4.0,<9.0.0").unwrap();
    assert_eq!(
        resolve_registry_json(
            range.registry().unwrap(),
            PackageKind::Tool,
            "linux_x86_64",
            response
        )
        .unwrap()
        .version,
        "8.4.0+2021r2-patch5"
    );
}

#[test]
fn platform_manifest_dependencies_obey_package_override_precedence() {
    let manifest = r#"{"packages":{
        "framework-arduinoespressif32":{"type":"framework","owner":"platformio","version":"~3.20017.0","optional":true},
        "toolchain-xtensa-esp32s3":{"type":"toolchain","owner":"espressif","version":"8.4.0+2021r2-patch5","optional":true}
    }}"#;
    let override_spec =
        parse_package_spec("framework-arduinoespressif32@3.20017.241212+sha.dcc1105b").unwrap();
    let requirements = resolve_platform_requirements(manifest, &[override_spec]).unwrap();
    let framework = requirements
        .iter()
        .find(|r| r.name == "framework-arduinoespressif32")
        .unwrap();
    assert_eq!(framework.kind, PackageKind::Framework);
    assert_eq!(
        framework.spec.registry().unwrap().requirement.as_deref(),
        Some("3.20017.241212+sha.dcc1105b")
    );
    let toolchain = requirements
        .iter()
        .find(|r| r.name == "toolchain-xtensa-esp32s3")
        .unwrap();
    assert_eq!(toolchain.kind, PackageKind::Tool);
    assert_eq!(
        toolchain.spec.registry().unwrap().owner.as_deref(),
        Some("espressif")
    );
}

#[test]
fn esp32s3_613_manifest_resolves_exact_framework_and_compiler_payloads_offline() {
    let manifest = r#"{"packages":{
        "framework-arduinoespressif32":{"type":"framework","owner":"platformio","version":"~3.20017.0","optional":true},
        "toolchain-xtensa-esp32s3":{"type":"toolchain","owner":"espressif","version":"8.4.0+2021r2-patch5","optional":true}
    }}"#;
    let requirements = resolve_platform_requirements(manifest, &[]).unwrap();
    let cases = [
        (
            "framework-arduinoespressif32",
            PackageKind::Framework,
            r#"{"name":"framework-arduinoespressif32","owner":{"username":"platformio"},"versions":[{"name":"3.20017.241212+sha.dcc1105b","files":[{"system":"*","download_url":"https://dl.registry.platformio.org/download/platformio/tool/framework-arduinoespressif32/3.20017.241212+sha.dcc1105b/framework-arduinoespressif32-3.20017.241212+sha.dcc1105b.tar.gz","checksum":{"sha256":"7dbcfb86f9dfd5ecf6c881ed8226239d9ef63f27e1e1a2ddd87a87762e2ffbc9"}}]}]}"#,
            "3.20017.241212+sha.dcc1105b",
            "7dbcfb86f9dfd5ecf6c881ed8226239d9ef63f27e1e1a2ddd87a87762e2ffbc9",
        ),
        (
            "toolchain-xtensa-esp32s3",
            PackageKind::Tool,
            r#"{"name":"toolchain-xtensa-esp32s3","owner":{"username":"espressif"},"versions":[{"name":"8.4.0+2021r2-patch5","files":[{"system":"linux_x86_64","download_url":"https://dl.registry.platformio.org/download/espressif/tool/toolchain-xtensa-esp32s3/8.4.0+2021r2-patch5/toolchain-xtensa-esp32s3-linux_x86_64-8.4.0+2021r2-patch5.tar.gz","checksum":{"sha256":"6618e8a91fca47da09c840a61bf008b1f06698553389c01c8788e595d8e84da9"}}]}]}"#,
            "8.4.0+2021r2-patch5",
            "6618e8a91fca47da09c840a61bf008b1f06698553389c01c8788e595d8e84da9",
        ),
    ];
    for (name, kind, metadata, version, sha256) in cases {
        let requirement = requirements.iter().find(|r| r.name == name).unwrap();
        let payload = resolve_registry_json(
            requirement.spec.registry().unwrap(),
            kind,
            "linux_x86_64",
            metadata,
        )
        .unwrap();
        assert_eq!(payload.version, version, "{name}");
        assert_eq!(payload.sha256, sha256, "{name}");
        assert!(
            payload
                .url
                .starts_with("https://dl.registry.platformio.org/download/")
        );
    }
}

#[test]
fn teensy_51_manifest_honors_explicit_framework_pin_offline() {
    let platform = parse_package_spec("platformio/teensy@5.1.0").unwrap();
    let platform_metadata = r#"{"name":"teensy","owner":{"username":"platformio"},"versions":[{"name":"5.1.0","files":[{"system":"*","download_url":"https://dl.registry.platformio.org/download/platformio/platform/teensy/5.1.0/teensy-5.1.0.tar.gz","checksum":{"sha256":"f129fd2d6acedf3f9fa513b4aefbe01d02bb6d10d386c6348ac684f24c89d46d"}}]}]}"#;
    let platform_payload = resolve_registry_json(
        platform.registry().unwrap(),
        PackageKind::Platform,
        "linux_x86_64",
        platform_metadata,
    )
    .unwrap();
    assert_eq!(platform_payload.version, "5.1.0");
    assert_eq!(
        platform_payload.sha256,
        "f129fd2d6acedf3f9fa513b4aefbe01d02bb6d10d386c6348ac684f24c89d46d"
    );

    let manifest = r#"{"packages":{
        "framework-arduinoteensy":{"type":"framework","owner":"platformio","version":"~1.160.0","optional":true},
        "toolchain-gccarmnoneeabi-teensy":{"type":"toolchain","owner":"platformio","version":"~1.110301.0","optional":false}
    }}"#;
    let framework_override = parse_package_spec("framework-arduinoteensy@1.159.0").unwrap();
    let requirements = resolve_platform_requirements(manifest, &[framework_override]).unwrap();
    let framework = requirements
        .iter()
        .find(|requirement| requirement.name == "framework-arduinoteensy")
        .unwrap();
    assert_eq!(
        framework.spec.registry().unwrap().owner.as_deref(),
        Some("platformio")
    );
    assert_eq!(
        framework.spec.registry().unwrap().requirement.as_deref(),
        Some("1.159.0")
    );
    let framework_metadata = r#"{"name":"framework-arduinoteensy","owner":{"username":"platformio"},"versions":[{"name":"1.159.0","files":[{"system":"*","download_url":"https://dl.registry.platformio.org/download/platformio/tool/framework-arduinoteensy/1.159.0/framework-arduinoteensy-1.159.0.tar.gz","checksum":{"sha256":"c511fa047471c656bc28cfe5e95eacb223f83cc7a3b31fcf652e25b5e73f7838"}}]}]}"#;
    let framework_payload = resolve_registry_json(
        framework.spec.registry().unwrap(),
        framework.kind,
        "linux_x86_64",
        framework_metadata,
    )
    .unwrap();
    assert_eq!(framework_payload.version, "1.159.0");
    assert_eq!(
        framework_payload.sha256,
        "c511fa047471c656bc28cfe5e95eacb223f83cc7a3b31fcf652e25b5e73f7838"
    );
    assert_ne!(
        framework_payload.cache_identity(),
        platform_payload.cache_identity()
    );
    let toolchain = requirements
        .iter()
        .find(|requirement| requirement.name == "toolchain-gccarmnoneeabi-teensy")
        .unwrap();
    assert_eq!(
        toolchain.spec.registry().unwrap().requirement.as_deref(),
        Some("~1.110301.0")
    );
}

#[test]
fn stm32_20_arduino_board_requirements_select_published_payloads_offline() {
    let platform = parse_package_spec("platformio/ststm32@20.0.0").unwrap();
    let platform_metadata = r#"{"name":"ststm32","owner":{"username":"platformio"},"versions":[{"name":"20.0.0","files":[{"system":"*","download_url":"https://dl.registry.platformio.org/download/platformio/platform/ststm32/20.0.0/ststm32-20.0.0.tar.gz","checksum":{"sha256":"2a41b41fd27831994b18653e41f730a9a55f37f29d7792819244c079f1fcda74"}}]}]}"#;
    let platform_payload = resolve_registry_json(
        platform.registry().unwrap(),
        PackageKind::Platform,
        "linux_x86_64",
        platform_metadata,
    )
    .unwrap();
    assert_eq!(platform_payload.version, "20.0.0");
    assert_eq!(
        platform_payload.sha256,
        "2a41b41fd27831994b18653e41f730a9a55f37f29d7792819244c079f1fcda74"
    );

    // platform-ststm32/platform.py selects GCC 12 and CMSIS 6 for a standard
    // Arduino board. The raw platform.json defaults are GCC 7/CMSIS 5.
    let manifest = r#"{"packages":{
        "framework-arduinoststm32":{"type":"framework","owner":"platformio","version":"~4.30000.0","optional":true},
        "framework-cmsis":{"type":"framework","owner":"platformio","version":"~2.50501.0","optional":true},
        "toolchain-gccarmnoneeabi":{"type":"toolchain","owner":"platformio","version":">=1.60301.0,<1.80000.0","optional":false}
    }}"#;
    let board_defaults = [
        parse_package_spec("toolchain-gccarmnoneeabi@~1.120301.0").unwrap(),
        parse_package_spec("framework-cmsis@~2.60300.0").unwrap(),
    ];
    let requirements = resolve_platform_requirements(manifest, &board_defaults).unwrap();
    let cases = [
        (
            "framework-arduinoststm32",
            "4.30000.0",
            r#"{"name":"framework-arduinoststm32","owner":{"username":"platformio"},"versions":[{"name":"4.30000.0","files":[{"system":"*","download_url":"https://dl.registry.platformio.org/download/platformio/tool/framework-arduinoststm32/4.30000.0/framework-arduinoststm32-4.30000.0.tar.gz","checksum":{"sha256":"ce42fefd75d7a183f3819fa1578de3c5a04db374201d117f5789d4a6a2d5dffe"}}]}]}"#,
            "ce42fefd75d7a183f3819fa1578de3c5a04db374201d117f5789d4a6a2d5dffe",
        ),
        (
            "framework-cmsis",
            "2.60300.0",
            r#"{"name":"framework-cmsis","owner":{"username":"platformio"},"versions":[{"name":"2.60300.0","files":[{"system":"*","download_url":"https://dl.registry.platformio.org/download/platformio/tool/framework-cmsis/2.60300.0/framework-cmsis-2.60300.0.tar.gz","checksum":{"sha256":"ca77d29356c77c2e45b7a5bb940fe1b431c9c1583d897ddc875b5dfcd9595398"}}]}]}"#,
            "ca77d29356c77c2e45b7a5bb940fe1b431c9c1583d897ddc875b5dfcd9595398",
        ),
        (
            "toolchain-gccarmnoneeabi",
            "1.120301.0",
            r#"{"name":"toolchain-gccarmnoneeabi","owner":{"username":"platformio"},"versions":[{"name":"1.120301.0","files":[{"system":"linux_x86_64","download_url":"https://dl.registry.platformio.org/download/platformio/tool/toolchain-gccarmnoneeabi/1.120301.0/toolchain-gccarmnoneeabi-linux_x86_64-1.120301.0.tar.gz","checksum":{"sha256":"d61c40c097032ea32c2fd1622e0fab72d2a6dc6ec9b69fa451a629eeb17448ed"}}]}]}"#,
            "d61c40c097032ea32c2fd1622e0fab72d2a6dc6ec9b69fa451a629eeb17448ed",
        ),
    ];
    for (name, version, metadata, sha256) in cases {
        let requirement = requirements
            .iter()
            .find(|requirement| requirement.name == name)
            .unwrap();
        let payload = resolve_registry_json(
            requirement.spec.registry().unwrap(),
            requirement.kind,
            "linux_x86_64",
            metadata,
        )
        .unwrap();
        assert_eq!(payload.version, version, "{name}");
        assert_eq!(payload.sha256, sha256, "{name}");
        assert_ne!(payload.cache_identity(), platform_payload.cache_identity());
    }
    let user_override = parse_package_spec("framework-cmsis@2.50900.0").unwrap();
    let requirements =
        resolve_platform_requirements(manifest, &[user_override, board_defaults[1].clone()])
            .unwrap();
    assert_eq!(
        requirements
            .iter()
            .find(|requirement| requirement.name == "framework-cmsis")
            .unwrap()
            .spec
            .registry()
            .unwrap()
            .requirement
            .as_deref(),
        Some("2.50900.0")
    );
}

#[test]
fn stm32_arduino_cmsis_requirement_follows_the_pinned_platform_manifest() {
    // platform-ststm32's Arduino builder selects the sole CMSIS optional
    // version. That changed between published platform releases 19 and 20.
    let releases = [("19.0.0", "~2.50900.0"), ("20.0.0", "~2.60300.0")];
    for (platform_version, expected_cmsis) in releases {
        let manifest = format!(
            r#"{{"version":"{platform_version}","packages":{{"framework-cmsis":{{"type":"framework","owner":"platformio","version":"~2.50501.0","optionalVersions":["{expected_cmsis}"]}}}}}}"#
        );
        assert_eq!(
            sole_optional_manifest_version(&manifest, "framework-cmsis").unwrap(),
            expected_cmsis,
            "ststm32@{platform_version}"
        );
    }
    let ambiguous = r#"{"packages":{"framework-cmsis":{"version":"~2.50501.0","optionalVersions":["~2.50900.0","~2.60300.0"]}}}"#;
    assert!(sole_optional_manifest_version(ambiguous, "framework-cmsis").is_err());
}

#[test]
fn nordicnrf52_11_adafruit_resolves_platform_and_host_packages_offline() {
    let platform = parse_package_spec("platformio/nordicnrf52@11.0.0").unwrap();
    let platform_metadata = r#"{"name":"nordicnrf52","owner":{"username":"platformio"},"versions":[{"name":"11.0.0","files":[{"system":"*","download_url":"https://dl.registry.platformio.org/download/platformio/platform/nordicnrf52/11.0.0/nordicnrf52-11.0.0.tar.gz","checksum":{"sha256":"94f2744925eb31e8287d2a36f5db34c0282957454a9111a15ee34ebdd93208e9"}}]}]}"#;
    let platform_payload = resolve_registry_json(
        platform.registry().unwrap(),
        PackageKind::Platform,
        "linux_x86_64",
        platform_metadata,
    )
    .unwrap();
    assert_eq!(platform_payload.version, "11.0.0");
    assert_eq!(
        platform_payload.sha256,
        "94f2744925eb31e8287d2a36f5db34c0282957454a9111a15ee34ebdd93208e9"
    );

    let manifest = r#"{"packages":{
        "framework-arduinoadafruitnrf52":{"type":"framework","owner":"platformio","version":"~1.10700.0","optional":true},
        "framework-cmsis":{"type":"framework","owner":"platformio","version":"~2.50700.0","optional":true},
        "toolchain-gccarmnoneeabi":{"type":"toolchain","owner":"platformio","version":">=1.60301.0,<1.80000.0","optional":false}
    }}"#;
    let requirements = resolve_platform_requirements(manifest, &[]).unwrap();
    let cases = [
        (
            "framework-arduinoadafruitnrf52",
            "1.10700.0",
            "*",
            "https://dl.registry.platformio.org/download/platformio/tool/framework-arduinoadafruitnrf52/1.10700.0/framework-arduinoadafruitnrf52-1.10700.0.tar.gz",
            "59b3013b372cbaa17f46f3cd5aa7d00154996063b2e9151add7384471de66713",
        ),
        (
            "framework-cmsis",
            "2.50700.210515",
            "*",
            "https://dl.registry.platformio.org/download/platformio/tool/framework-cmsis/2.50700.210515/framework-cmsis-2.50700.210515.tar.gz",
            "c45aee42cad60ce1167b3ee15f36f624bb0d9878d831d3d4e32665c47d9635bb",
        ),
        (
            "toolchain-gccarmnoneeabi",
            "1.70201.0",
            "linux_x86_64",
            "https://dl.registry.platformio.org/download/platformio/tool/toolchain-gccarmnoneeabi/1.70201.0/toolchain-gccarmnoneeabi-linux_x86_64-1.70201.0.tar.gz",
            "26977183521a65bc2be43a81a6dabda0337430e104841078b20efba3fd0fddef",
        ),
    ];
    for (name, version, system, url, sha256) in cases {
        let kind = if name.starts_with("toolchain-") {
            PackageKind::Tool
        } else {
            PackageKind::Framework
        };
        let metadata = format!(
            r#"{{"name":"{name}","owner":{{"username":"platformio"}},"versions":[{{"name":"{version}","files":[{{"system":"{system}","download_url":"{url}","checksum":{{"sha256":"{sha256}"}}}}]}}]}}"#
        );
        let requirement = requirements.iter().find(|item| item.name == name).unwrap();
        let payload = resolve_registry_json(
            requirement.spec.registry().unwrap(),
            kind,
            "linux_x86_64",
            &metadata,
        )
        .unwrap();
        assert_eq!(payload.version, version, "{name}");
        assert_eq!(payload.url, url, "{name}");
        assert_eq!(payload.sha256, sha256, "{name}");
        assert_ne!(payload.cache_identity(), platform_payload.cache_identity());
    }

    let explicit = parse_package_spec("framework-cmsis@2.50900.0").unwrap();
    let requirements = resolve_platform_requirements(manifest, &[explicit]).unwrap();
    assert_eq!(
        requirements
            .iter()
            .find(|item| item.name == "framework-cmsis")
            .unwrap()
            .spec
            .registry()
            .unwrap()
            .requirement
            .as_deref(),
        Some("2.50900.0")
    );
}

#[test]
fn atmelsam_9_due_resolves_published_sam_stack_offline() {
    let platform = parse_package_spec("platformio/atmelsam@9.0.0").unwrap();
    let platform_url = "https://dl.registry.platformio.org/download/platformio/platform/atmelsam/9.0.0/atmelsam-9.0.0.tar.gz";
    let platform_sha = "ce12b1b2d1b2a0c776c2bbb932e4987cfd5072131ac1860d195874bff78862f9";
    let platform_metadata = format!(
        r#"{{"name":"atmelsam","owner":{{"username":"platformio"}},"versions":[{{"name":"9.0.0","files":[{{"system":"*","download_url":"{platform_url}","checksum":{{"sha256":"{platform_sha}"}}}}]}}]}}"#
    );
    let platform_payload = resolve_registry_json(
        platform.registry().unwrap(),
        PackageKind::Platform,
        "linux_x86_64",
        &platform_metadata,
    )
    .unwrap();
    assert_eq!(platform_payload.url, platform_url);
    assert_eq!(platform_payload.sha256, platform_sha);

    let manifest = r#"{"packages":{
        "toolchain-gccarmnoneeabi":{"type":"toolchain","owner":"platformio","version":"~1.70201.0"},
        "framework-arduino-sam":{"type":"framework","owner":"platformio","version":"~1.6.12","optional":true},
        "framework-arduino-samd":{"type":"framework","owner":"platformio","version":"~1.8.14","optional":true},
        "framework-arduino-samd-adafruit":{"type":"framework","owner":"platformio","version":"~1.10716.0","optional":true},
        "framework-cmsis":{"type":"framework","owner":"platformio","version":"~1.40500.0","optional":true},
        "framework-cmsis-atmel":{"type":"framework","owner":"platformio","version":"~1.2.2","optional":true}
    }}"#;
    let requirements = resolve_platform_requirements(manifest, &[]).unwrap();
    let cases = [
        (
            "framework-arduino-sam",
            "1.6.12",
            "*",
            "https://dl.registry.platformio.org/download/platformio/tool/framework-arduino-sam/1.6.12/framework-arduino-sam-1.6.12.tar.gz",
            "c657856d3aa8e8355c2feac26b8865b580725a914ac614a0b8c1d5fdd27494b9",
        ),
        (
            "framework-cmsis",
            "1.40500.0",
            "*",
            "https://dl.registry.platformio.org/download/platformio/tool/framework-cmsis/1.40500.0/framework-cmsis-1.40500.0.tar.gz",
            "ec073dc0a74311fb4a63b2ba1a011e37491d494f1ef2abf87ec233c14a654f19",
        ),
        (
            "framework-cmsis-atmel",
            "1.2.2",
            "*",
            "https://dl.registry.platformio.org/download/platformio/tool/framework-cmsis-atmel/1.2.2/framework-cmsis-atmel-1.2.2.tar.gz",
            "de7778f049e2558e1c8dbb50478f47c5680d4b393bc49e702203ffdd7a6ca60c",
        ),
        (
            "toolchain-gccarmnoneeabi",
            "1.70201.0",
            "linux_x86_64",
            "https://dl.registry.platformio.org/download/platformio/tool/toolchain-gccarmnoneeabi/1.70201.0/toolchain-gccarmnoneeabi-linux_x86_64-1.70201.0.tar.gz",
            "26977183521a65bc2be43a81a6dabda0337430e104841078b20efba3fd0fddef",
        ),
    ];
    for (name, version, system, url, sha256) in cases {
        let requirement = requirements.iter().find(|item| item.name == name).unwrap();
        let metadata = format!(
            r#"{{"name":"{name}","owner":{{"username":"platformio"}},"versions":[{{"name":"{version}","files":[{{"system":"{system}","download_url":"{url}","checksum":{{"sha256":"{sha256}"}}}}]}}]}}"#
        );
        let payload = resolve_registry_json(
            requirement.spec.registry().unwrap(),
            requirement.kind,
            "linux_x86_64",
            &metadata,
        )
        .unwrap();
        assert_eq!(payload.version, version, "{name}");
        assert_eq!(payload.url, url, "{name}");
        assert_eq!(payload.sha256, sha256, "{name}");
        assert_ne!(payload.cache_identity(), platform_payload.cache_identity());
    }

    let explicit = parse_package_spec("framework-arduino-sam@1.6.11").unwrap();
    let requirements = resolve_platform_requirements(manifest, &[explicit]).unwrap();
    assert_eq!(
        requirements
            .iter()
            .find(|item| item.name == "framework-arduino-sam")
            .unwrap()
            .spec
            .registry()
            .unwrap()
            .requirement
            .as_deref(),
        Some("1.6.11")
    );

    // platform-atmelsam/platform.py selects these requirements for an
    // Adafruit SAMD board instead of the manifest's Arduino-core defaults.
    let adafruit_defaults = [
        parse_package_spec("toolchain-gccarmnoneeabi@~1.90301.0").unwrap(),
        parse_package_spec("framework-cmsis@~2.50400.0").unwrap(),
    ];
    let requirements = resolve_platform_requirements(manifest, &adafruit_defaults).unwrap();
    let cases = [
        (
            "framework-arduino-samd-adafruit",
            "1.10716.0",
            "*",
            "https://dl.registry.platformio.org/download/platformio/tool/framework-arduino-samd-adafruit/1.10716.0/framework-arduino-samd-adafruit-1.10716.0.tar.gz",
            "f5266198218d316e62f205653b054c103cc4eb971bb689e7d699857aa80c267e",
        ),
        (
            "framework-cmsis",
            "2.50400.181126",
            "*",
            "https://dl.registry.platformio.org/download/platformio/tool/framework-cmsis/2.50400.181126/framework-cmsis-2.50400.181126.tar.gz",
            "f38dacbdb00eaca555126be8fcc5d09a41a16e194e2a8564f9a37845dda4373e",
        ),
        (
            "toolchain-gccarmnoneeabi",
            "1.90301.200702",
            "linux_x86_64",
            "https://dl.registry.platformio.org/download/platformio/tool/toolchain-gccarmnoneeabi/1.90301.200702/toolchain-gccarmnoneeabi-linux_x86_64-1.90301.200702.tar.gz",
            "fbbc57fe1560fbe6e1d5890a934258f6b1439fc976f3ef584d12bb8aae9b3c7d",
        ),
    ];
    for (name, version, system, url, sha256) in cases {
        let requirement = requirements.iter().find(|item| item.name == name).unwrap();
        let metadata = format!(
            r#"{{"name":"{name}","owner":{{"username":"platformio"}},"versions":[{{"name":"{version}","files":[{{"system":"{system}","download_url":"{url}","checksum":{{"sha256":"{sha256}"}}}}]}}]}}"#
        );
        let payload = resolve_registry_json(
            requirement.spec.registry().unwrap(),
            requirement.kind,
            "linux_x86_64",
            &metadata,
        )
        .unwrap();
        assert_eq!(payload.url, url, "{name}");
        assert_eq!(payload.sha256, sha256, "{name}");
        assert_ne!(payload.cache_identity(), platform_payload.cache_identity());
    }
}

#[test]
fn clearcore_requires_explicit_framework_source_not_an_atmelsam_default() {
    // The official atmelsam manifest has no ClearCore framework declaration.
    // A valid platform payload must not manufacture one from native defaults.
    let manifest = r#"{"packages":{
        "toolchain-gccarmnoneeabi":{"type":"toolchain","owner":"platformio","version":"~1.70201.0"},
        "framework-arduino-sam":{"type":"framework","owner":"platformio","version":"~1.6.12","optional":true},
        "framework-cmsis":{"type":"framework","owner":"platformio","version":"~1.40500.0","optional":true}
    }}"#;
    let default_requirements = resolve_platform_requirements(manifest, &[]).unwrap();
    assert_eq!(
        require_platform_package(&default_requirements, "framework-arduino-sam-clearcore")
            .unwrap_err(),
        ResolutionError::MissingPackage("framework-arduino-sam-clearcore".into())
    );

    let explicit = parse_package_spec(
        "framework-arduino-sam-clearcore@https://www.teknic.com/files/downloads/ClearCore-1.7.4.zip",
    )
    .unwrap();
    let requirements = resolve_platform_requirements(manifest, &[explicit]).unwrap();
    let framework =
        require_platform_package(&requirements, "framework-arduino-sam-clearcore").unwrap();
    assert!(matches!(
        &framework.spec.source,
        PackageSource::Archive { url, revision: None }
            if url == "https://www.teknic.com/files/downloads/ClearCore-1.7.4.zip"
    ));
    let locked = PackageLock::Archive {
        url: "https://www.teknic.com/files/downloads/ClearCore-1.7.4.zip".into(),
        sha256: "87542411133e8b1b0bb88d12a5df6601c8054b61e213e358fa95bb08e8632270".into(),
    };
    assert_ne!(
        locked.cache_identity(),
        PackageLock::Archive {
            url: "https://www.teknic.com/files/downloads/ClearCore-1.7.4.zip".into(),
            sha256: "0".repeat(64),
        }
        .cache_identity()
    );
}

#[test]
fn custom_arduino_arm_families_do_not_substitute_official_registry_manifests() {
    // These official PlatformIO platform releases are valid payloads, but
    // their manifests do not declare the custom Arduino cores used by fbuild's
    // RP, LPC8xx, and Silicon Labs adapters. Core resolution succeeds; the
    // required framework lookup must fail independently of native dispatch.
    let cases = [
        (
            "raspberrypi",
            "1.20.0",
            "80ffdadda508a7ad7973603f22e3cdf86f55ac1dfcc5b5afee1d87e46698e031",
            "framework-arduinopico",
            r#"{"packages":{"framework-arduino-mbed":{"type":"framework","owner":"platformio","version":"~4.6.0","optional":true},"toolchain-gccarmnoneeabi":{"type":"toolchain","owner":"platformio","version":"~1.90201.0"}}}"#,
        ),
        (
            "nxplpc",
            "11.0.0",
            "e51b2c50b2f9797c3d8881442cbe2551973ae79f8d9e20052a616e36bdbb0686",
            "framework-arduino-lpc8xx",
            r#"{"packages":{"framework-mbed":{"type":"framework","owner":"platformio","version":"~6.61700.0","optional":true},"toolchain-gccarmnoneeabi":{"type":"toolchain","owner":"platformio","version":"~1.120301.0"}}}"#,
        ),
        (
            "siliconlabsefm32",
            "11.0.0",
            "42673c84bcad9d079961df6406258d60186feee312ac8befb08116c0d19b233f",
            "framework-arduino-silabs",
            r#"{"packages":{"framework-mbed":{"type":"framework","owner":"platformio","version":"~6.61700.0","optional":true},"toolchain-gccarmnoneeabi":{"type":"toolchain","owner":"platformio","version":"~1.120301.0"}}}"#,
        ),
    ];
    for (name, version, sha256, native_framework, manifest) in cases {
        let spec = parse_package_spec(&format!("platformio/platform/{name}@{version}")).unwrap();
        let url = format!(
            "https://dl.registry.platformio.org/download/platformio/platform/{name}/{version}/{name}-{version}.tar.gz"
        );
        let metadata = format!(
            r#"{{"name":"{name}","owner":{{"username":"platformio"}},"versions":[{{"name":"{version}","files":[{{"system":"*","download_url":"{url}","checksum":{{"sha256":"{sha256}"}}}}]}}]}}"#
        );
        let payload = resolve_registry_json(
            spec.registry().unwrap(),
            PackageKind::Platform,
            "linux_x86_64",
            &metadata,
        )
        .unwrap();
        assert_eq!(payload.url, url, "{name}");
        assert_eq!(payload.sha256, sha256, "{name}");
        assert!(!payload.cache_identity().is_empty(), "{name}");

        let requirements = resolve_platform_requirements(manifest, &[]).unwrap();
        assert_eq!(
            require_platform_package(&requirements, native_framework).unwrap_err(),
            ResolutionError::MissingPackage(native_framework.into()),
            "{name}"
        );
        let toolchain =
            require_platform_package(&requirements, "toolchain-gccarmnoneeabi").unwrap();
        assert_eq!(
            toolchain.spec.registry().unwrap().owner.as_deref(),
            Some("platformio"),
            "{name}"
        );

        // A custom platform source may provide an explicit Arduino package;
        // that source identity remains distinct from the official manifest.
        let explicit = parse_package_spec(&format!(
            "{native_framework}@https://example.test/{native_framework}.tar.gz"
        ))
        .unwrap();
        let requirements = resolve_platform_requirements(manifest, &[explicit]).unwrap();
        assert!(matches!(
            &require_platform_package(&requirements, native_framework)
                .unwrap()
                .spec
                .source,
            PackageSource::Archive { .. }
        ));
    }
}

#[test]
fn renesas_ra_19_uno_r4_resolves_published_stack_offline() {
    let platform = parse_package_spec("platformio/renesas-ra@1.9.0").unwrap();
    let platform_url = "https://dl.registry.platformio.org/download/platformio/platform/renesas-ra/1.9.0/renesas-ra-1.9.0.tar.gz";
    let platform_sha = "f84ff1366c88e16e2feabf4a1355f270f85c3815fffd13abe6fde92a8e15533a";
    let metadata = format!(
        r#"{{"name":"renesas-ra","owner":{{"username":"platformio"}},"versions":[{{"name":"1.9.0","files":[{{"system":"*","download_url":"{platform_url}","checksum":{{"sha256":"{platform_sha}"}}}}]}}]}}"#
    );
    let platform_payload = resolve_registry_json(
        platform.registry().unwrap(),
        PackageKind::Platform,
        "linux_x86_64",
        &metadata,
    )
    .unwrap();
    assert_eq!(platform_payload.url, platform_url);
    assert_eq!(platform_payload.sha256, platform_sha);

    let manifest = r#"{"packages":{
        "framework-arduinorenesas-uno":{"type":"framework","owner":"platformio","version":"~1.6.0","optional":true},
        "framework-renesas-fsp":{"type":"framework","owner":"platformio","version":"1.40000.0","optional":true},
        "framework-cmsis-renesas":{"type":"framework","owner":"platformio","version":"1.40500.0","optional":true},
        "toolchain-gccarmnoneeabi":{"type":"toolchain","owner":"platformio","version":"~1.70201.0"}
    }}"#;
    let requirements = resolve_platform_requirements(manifest, &[]).unwrap();
    let cases = [
        (
            "framework-arduinorenesas-uno",
            "1.6.0",
            "*",
            "https://dl.registry.platformio.org/download/platformio/tool/framework-arduinorenesas-uno/1.6.0/framework-arduinorenesas-uno-1.6.0.tar.gz",
            "3e55bb831d8ab6a1a34137470980ef5ac65d18418f5083037f2fe569f60c1252",
        ),
        (
            "toolchain-gccarmnoneeabi",
            "1.70201.0",
            "linux_x86_64",
            "https://dl.registry.platformio.org/download/platformio/tool/toolchain-gccarmnoneeabi/1.70201.0/toolchain-gccarmnoneeabi-linux_x86_64-1.70201.0.tar.gz",
            "26977183521a65bc2be43a81a6dabda0337430e104841078b20efba3fd0fddef",
        ),
    ];
    for (name, version, system, url, sha256) in cases {
        let requirement = requirements.iter().find(|item| item.name == name).unwrap();
        let metadata = format!(
            r#"{{"name":"{name}","owner":{{"username":"platformio"}},"versions":[{{"name":"{version}","files":[{{"system":"{system}","download_url":"{url}","checksum":{{"sha256":"{sha256}"}}}}]}}]}}"#
        );
        let payload = resolve_registry_json(
            requirement.spec.registry().unwrap(),
            requirement.kind,
            "linux_x86_64",
            &metadata,
        )
        .unwrap();
        assert_eq!(payload.url, url, "{name}");
        assert_eq!(payload.sha256, sha256, "{name}");
        assert_ne!(payload.cache_identity(), platform_payload.cache_identity());
    }
    let explicit = parse_package_spec("framework-arduinorenesas-uno@1.5.0").unwrap();
    let requirements = resolve_platform_requirements(manifest, &[explicit]).unwrap();
    assert_eq!(
        requirements
            .iter()
            .find(|item| item.name == "framework-arduinorenesas-uno")
            .unwrap()
            .spec
            .registry()
            .unwrap()
            .requirement
            .as_deref(),
        Some("1.5.0")
    );
}

#[test]
fn apollo3_repository_platform_keeps_source_and_resolves_toolchain_payload_offline() {
    let platform = parse_package_spec("https://github.com/nigelb/platform-apollo3blue").unwrap();
    assert!(matches!(platform.source, PackageSource::Repository { .. }));
    let toolchain =
        parse_package_spec("platformio/tool/toolchain-gccarmnoneeabi@1.90201.191206").unwrap();
    let url = "https://dl.registry.platformio.org/download/platformio/tool/toolchain-gccarmnoneeabi/1.90201.191206/toolchain-gccarmnoneeabi-linux_x86_64-1.90201.191206.tar.gz";
    let sha256 = "140fb263798b9dc1950b3831c44d9ab01196f883012b78658b1e002b9035d26c";
    let metadata = format!(
        r#"{{"name":"toolchain-gccarmnoneeabi","owner":{{"username":"platformio"}},"versions":[{{"name":"1.90201.191206","files":[{{"system":"linux_x86_64","download_url":"{url}","checksum":{{"sha256":"{sha256}"}}}}]}}]}}"#
    );
    let payload = resolve_registry_json(
        toolchain.registry().unwrap(),
        PackageKind::Tool,
        "linux_x86_64",
        &metadata,
    )
    .unwrap();
    assert_eq!(payload.url, url);
    assert_eq!(payload.sha256, sha256);
    assert_eq!(payload.version, "1.90201.191206");
    assert!(!payload.cache_identity().is_empty());
}

#[test]
fn generic_package_paths_and_kinds_do_not_depend_on_native_platforms() {
    let cases = [
        ("acme/custom-board@1.2.3", Some("acme"), "custom-board"),
        (
            "platformio/espressif32@6.13.0",
            Some("platformio"),
            "espressif32",
        ),
        ("framework-foo@2.0.0", None, "framework-foo"),
        ("toolchain-foo@^3.0.0", None, "toolchain-foo"),
    ];
    for (raw, owner, name) in cases {
        let package = parse_package_spec(raw).unwrap();
        let registry = package.registry().unwrap();
        assert_eq!(registry.owner.as_deref(), owner, "{raw}");
        assert_eq!(registry.name, name, "{raw}");
    }
    assert!(
        registry_api_url(
            PackageKind::Platform,
            parse_package_spec("custom-board@1.2.3")
                .unwrap()
                .registry()
                .unwrap()
        )
        .is_err()
    );
    assert_eq!(PackageKind::Framework.registry_type(), "tool");
    assert_eq!(PackageKind::Library.registry_type(), "library");
}

#[test]
fn malformed_inputs_and_missing_versions_fail_without_default_substitution() {
    for raw in [
        "",
        "file://",
        "acme//bad@1.0.0",
        "acme/custom@",
        "custom@not-a-version",
        "ftp://host/package.tar.gz",
    ] {
        assert!(parse_package_spec(raw).is_err(), "{raw}");
    }
    let spec = parse_package_spec("acme/custom-board@1.2.4").unwrap();
    let metadata = r#"{"name":"custom-board","owner":{"username":"acme"},"versions":[{"name":"1.2.3","files":[{"system":"*","download_url":"https://example.test/custom.tar.gz","checksum":{"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}]}]}"#;
    assert!(
        resolve_registry_json(
            spec.registry().unwrap(),
            PackageKind::Platform,
            "linux_x86_64",
            metadata
        )
        .is_err()
    );
}

#[test]
fn resolved_digest_and_host_are_part_of_cache_identity() {
    let metadata = r#"{"name":"custom-board","owner":{"username":"acme"},"versions":[{"name":"1.2.3","files":[{"system":"*","download_url":"https://example.test/custom.tar.gz","checksum":{"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}]}]}"#;
    let spec = parse_package_spec("acme/custom-board@1.2.3").unwrap();
    let payload = resolve_registry_json(
        spec.registry().unwrap(),
        PackageKind::Platform,
        "linux_x86_64",
        metadata,
    )
    .unwrap();
    let mut different_digest = payload.clone();
    different_digest.sha256 =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
    let mut different_host = payload.clone();
    different_host.system = "windows_amd64".into();
    assert_ne!(payload.cache_identity(), different_digest.cache_identity());
    assert_ne!(payload.cache_identity(), different_host.cache_identity());
    assert_eq!(payload.cache_identity(), payload.clone().cache_identity());
}

#[test]
fn non_registry_locks_are_content_addressed() {
    let url = "https://example.test/platform.tar.gz".to_string();
    let first = PackageLock::Archive {
        url: url.clone(),
        sha256: "a".repeat(64),
    };
    let second = PackageLock::Archive {
        url: url.clone(),
        sha256: "b".repeat(64),
    };
    let repo = PackageLock::Repository {
        url: "https://github.com/acme/platform.git".into(),
        commit: "deadbeef".into(),
    };
    let local = PackageLock::LocalPath {
        path: "../platforms/custom".into(),
        sha256: "a".repeat(64),
    };
    assert_ne!(first.cache_identity(), second.cache_identity());
    assert_ne!(first.cache_identity(), repo.cache_identity());
    assert_ne!(first.cache_identity(), local.cache_identity());
    assert_eq!(first.cache_identity(), first.clone().cache_identity());
}

#[test]
fn native_host_maps_to_platformio_registry_system_without_substitution() {
    use fbuild_core::platform::host::{HostArch, HostOs, HostPlatform};
    use fbuild_core::platformio_package::host_system;

    for (os, arch, expected) in [
        (HostOs::Linux, HostArch::X86_64, "linux_x86_64"),
        (HostOs::Linux, HostArch::Aarch64, "linux_aarch64"),
        (HostOs::Windows, HostArch::X86_64, "windows_amd64"),
        (HostOs::Windows, HostArch::Aarch64, "windows_arm64"),
        (HostOs::Macos, HostArch::X86_64, "darwin_x86_64"),
        (HostOs::Macos, HostArch::Aarch64, "darwin_arm64"),
    ] {
        assert_eq!(host_system(HostPlatform::new(os, arch)), Some(expected));
    }
    assert_eq!(
        host_system(HostPlatform::new(HostOs::Linux, HostArch::Other)),
        None
    );
}
