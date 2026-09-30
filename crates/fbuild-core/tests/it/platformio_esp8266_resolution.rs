//! Offline ESP8266 registry fixtures from platformio/espressif8266@4.0.1.

use fbuild_core::platformio_package::{
    PackageKind, parse_package_spec, require_platform_package, resolve_platform_requirements,
    resolve_registry_json,
};

const PLATFORM_METADATA: &str = r#"{"name":"espressif8266","owner":{"username":"platformio"},"versions":[{"name":"4.0.1","files":[{"system":"*","download_url":"https://dl.registry.platformio.org/download/platformio/platform/espressif8266/4.0.1/espressif8266-4.0.1.tar.gz","checksum":{"sha256":"a5a0fbbab19d1c993edd95b782a6c29e70b585c0140d93009b4e0bc8347df15c"}}]}]}"#;
const MANIFEST: &str = r#"{
  "version":"4.0.1",
  "packages":{
    "toolchain-xtensa":{"type":"toolchain","owner":"platformio","version":"~2.100300.0","optionalVersions":["~1.40802.0"]},
    "framework-arduinoespressif8266":{"type":"framework","optional":true,"owner":"platformio","version":"~3.30002.0"},
    "framework-esp8266-rtos-sdk":{"type":"framework","optional":true,"owner":"platformio","version":">=1.5.0-beta"},
    "framework-esp8266-nonos-sdk":{"type":"framework","optional":true,"owner":"platformio","version":">=2.1.0"},
    "tool-esptool":{"type":"uploader","owner":"platformio","version":"<2"},
    "tool-esptoolpy":{"type":"uploader","owner":"platformio","version":"~1.30000.0"},
    "tool-mkspiffs":{"type":"uploader","optional":true,"owner":"platformio","version":"~1.200.0"},
    "tool-mklittlefs":{"type":"uploader","optional":true,"owner":"platformio","version":"~1.203.0"}
  }
}"#;
const FRAMEWORK_METADATA: &str = r#"{"name":"framework-arduinoespressif8266","owner":{"username":"platformio"},"versions":[{"name":"3.30002.0","files":[{"system":"*","download_url":"https://dl.registry.platformio.org/download/platformio/tool/framework-arduinoespressif8266/3.30002.0/framework-arduinoespressif8266-3.30002.0.tar.gz","checksum":{"sha256":"ba4bf3467a4b09d32d73ea06fad0864dac3f84523bb02c27a3a97b66d593fa65"}}]}]}"#;
const TOOLCHAIN_METADATA: &str = r#"{"name":"toolchain-xtensa","owner":{"username":"platformio"},"versions":[{"name":"2.100300.220621","files":[{"system":["linux_x86_64"],"download_url":"https://dl.registry.platformio.org/download/platformio/tool/toolchain-xtensa/2.100300.220621/toolchain-xtensa-linux_x86_64-2.100300.220621.tar.gz","checksum":{"sha256":"a3d51bebcfaa2f5cca154956fee3e9270b6d0e9c5d51de6034a86aaa606ea8a5"}}]}]}"#;

#[test]
fn espressif8266_aliases_resolve_exact_platform_and_packages() {
    let mut platform_identity = None;
    for alias in [
        "espressif8266@4.0.1",
        "platformio/espressif8266@4.0.1",
        "platform/platformio/espressif8266@4.0.1",
    ] {
        let spec = parse_package_spec(alias).unwrap();
        let payload = resolve_registry_json(
            spec.registry().unwrap(),
            PackageKind::Platform,
            "linux_x86_64",
            PLATFORM_METADATA,
        )
        .unwrap();
        assert_eq!(payload.version, "4.0.1");
        assert_eq!(
            payload.sha256,
            "a5a0fbbab19d1c993edd95b782a6c29e70b585c0140d93009b4e0bc8347df15c"
        );
        assert_eq!(
            payload.url,
            "https://dl.registry.platformio.org/download/platformio/platform/espressif8266/4.0.1/espressif8266-4.0.1.tar.gz"
        );
        assert_eq!(
            platform_identity.get_or_insert_with(|| payload.cache_identity()),
            &payload.cache_identity()
        );
    }

    let requirements = resolve_platform_requirements(MANIFEST, &[]).unwrap();
    let framework =
        require_platform_package(&requirements, "framework-arduinoespressif8266").unwrap();
    assert_eq!(framework.kind, PackageKind::Framework);
    let framework_payload = resolve_registry_json(
        framework.spec.registry().unwrap(),
        framework.kind,
        "linux_x86_64",
        FRAMEWORK_METADATA,
    )
    .unwrap();
    assert_eq!(framework_payload.version, "3.30002.0");
    assert_eq!(
        framework_payload.sha256,
        "ba4bf3467a4b09d32d73ea06fad0864dac3f84523bb02c27a3a97b66d593fa65"
    );
    let toolchain = require_platform_package(&requirements, "toolchain-xtensa").unwrap();
    assert_eq!(toolchain.kind, PackageKind::Tool);
    let toolchain_payload = resolve_registry_json(
        toolchain.spec.registry().unwrap(),
        toolchain.kind,
        "linux_x86_64",
        TOOLCHAIN_METADATA,
    )
    .unwrap();
    assert_eq!(toolchain_payload.version, "2.100300.220621");
    assert_eq!(
        toolchain_payload.sha256,
        "a3d51bebcfaa2f5cca154956fee3e9270b6d0e9c5d51de6034a86aaa606ea8a5"
    );
    assert_ne!(
        framework_payload.cache_identity(),
        toolchain_payload.cache_identity()
    );
    assert_ne!(
        platform_identity.as_ref().unwrap(),
        &framework_payload.cache_identity()
    );
    assert_ne!(
        platform_identity.unwrap(),
        toolchain_payload.cache_identity()
    );
    assert!(
        resolve_registry_json(
            toolchain.spec.registry().unwrap(),
            toolchain.kind,
            "freebsd_x86_64",
            TOOLCHAIN_METADATA
        )
        .is_err()
    );
}

#[test]
fn unavailable_esp8266_versions_and_host_do_not_fall_back() {
    let unavailable_platform = parse_package_spec("espressif8266@4.0.2").unwrap();
    assert!(
        resolve_registry_json(
            unavailable_platform.registry().unwrap(),
            PackageKind::Platform,
            "linux_x86_64",
            PLATFORM_METADATA
        )
        .is_err()
    );
    let unavailable_toolchain =
        parse_package_spec("platformio/toolchain-xtensa@2.100300.220622").unwrap();
    assert!(
        resolve_registry_json(
            unavailable_toolchain.registry().unwrap(),
            PackageKind::Tool,
            "linux_x86_64",
            TOOLCHAIN_METADATA
        )
        .is_err()
    );
}

#[test]
fn explicit_framework_pin_precedes_esp8266_manifest_requirement() {
    let override_spec =
        parse_package_spec("tool/platformio/framework-arduinoespressif8266@3.30002.0").unwrap();
    let requirements = resolve_platform_requirements(MANIFEST, &[override_spec]).unwrap();
    let framework =
        require_platform_package(&requirements, "framework-arduinoespressif8266").unwrap();
    assert_eq!(
        framework.spec.registry().unwrap().requirement.as_deref(),
        Some("3.30002.0")
    );
}
