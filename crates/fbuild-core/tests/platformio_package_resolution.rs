//! Issue #1491: PlatformIO package specs must resolve independently of native
//! platform dispatch. These fixtures are offline and intentionally include a
//! platform name that fbuild cannot build.

use fbuild_core::platformio_package::{
    PackageKind, PackageLock, PackageSource, parse_package_spec, registry_api_url,
    resolve_platform_requirements, resolve_registry_json,
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
    let file_url = parse_package_spec("file:///tmp/custom-platform").unwrap();

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
        matches!(spec.source, PackageSource::LocalPath { path } if path == std::path::PathBuf::from(r"C:\platforms\custom"))
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
