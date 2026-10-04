//! Offline AVR and megaAVR PlatformIO registry payload fixtures.

use fbuild_core::platformio_package::{
    PackageKind, parse_package_spec, require_platform_package, resolve_platform_requirements,
    resolve_registry_json,
};

#[test]
#[expect(clippy::too_many_lines, reason = "baseline, zackees/ci.yml#229")]
fn avr_and_megaavr_board_packages_resolve_exact_payloads_offline() {
    // Published registry metadata and platform manifests, captured 2026-09-27.
    // These generic-path tests deliberately do not dispatch on Platform.
    let cases = [
        (
            "atmelavr",
            "5.3.0",
            "370a482b8cd980b8954c03cec3e856f50758cac695491e6b6c8327f5e60079ac",
            "framework-arduino-avr-minicore",
            "3.1.2",
            "3cc43553f35d2d00a277d488eefba628816104ae61a9ca9cecd60470f555c9f6",
            "~3.1.0",
            "~1.70300.0",
        ),
        (
            "atmelavr",
            "5.3.0",
            "370a482b8cd980b8954c03cec3e856f50758cac695491e6b6c8327f5e60079ac",
            "framework-arduino-avr",
            "5.4.0",
            "bf85bcca114bad389fec51fecbf9b66821a233b366bd3f85cdb1cdcba6a28659",
            "~5.4.0",
            "~1.70300.0",
        ),
        (
            "atmelavr",
            "5.3.0",
            "370a482b8cd980b8954c03cec3e856f50758cac695491e6b6c8327f5e60079ac",
            "framework-arduino-avr-attiny",
            "1.5.2",
            "06665ec79058cd180b82d76a2bc6bd30fc5311d9eebaed56dc57389f325822ee",
            "~1.5.2",
            "~1.70300.0",
        ),
        (
            "atmelmegaavr",
            "1.10.0",
            "36f2d93a3865dcc543668f260479973ec70a15f120ba97bb130bc320ab8b4f08",
            "framework-arduino-megaavr",
            "1.8.8",
            "7f13029f3a4621c89c4676bd6192f39ef5de3b9dd77316e9931d9b07e56411d7",
            "~1.8.8",
            "~1.70300.0",
        ),
    ];
    for (
        platform_name,
        platform_version,
        platform_sha,
        framework_name,
        framework_version,
        framework_sha,
        framework_range,
        toolchain_range,
    ) in cases
    {
        let platform = parse_package_spec(&format!(
            "platformio/platform/{platform_name}@{platform_version}"
        ))
        .unwrap();
        let platform_url = format!(
            "https://dl.registry.platformio.org/download/platformio/platform/{platform_name}/{platform_version}/{platform_name}-{platform_version}.tar.gz"
        );
        let platform_metadata = format!(
            r#"{{"name":"{platform_name}","owner":{{"username":"platformio"}},"versions":[{{"name":"{platform_version}","files":[{{"system":"*","download_url":"{platform_url}","checksum":{{"sha256":"{platform_sha}"}}}}]}}]}}"#
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

        let manifest = format!(
            r#"{{"packages":{{"{framework_name}":{{"type":"framework","owner":"platformio","version":"{framework_range}","optional":true}},"toolchain-atmelavr":{{"type":"toolchain","owner":"platformio","version":"{toolchain_range}"}}}}}}"#
        );
        let override_spec = parse_package_spec(&format!(
            "tool/platformio/{framework_name}@{framework_version}"
        ))
        .unwrap();
        let requirements = resolve_platform_requirements(&manifest, &[override_spec]).unwrap();
        let framework = require_platform_package(&requirements, framework_name).unwrap();
        assert_eq!(
            framework.spec.registry().unwrap().requirement.as_deref(),
            Some(framework_version)
        );
        assert_eq!(framework.kind, PackageKind::Framework);
        let framework_url = format!(
            "https://dl.registry.platformio.org/download/platformio/tool/{framework_name}/{framework_version}/{framework_name}-{framework_version}.tar.gz"
        );
        let framework_metadata = format!(
            r#"{{"name":"{framework_name}","owner":{{"username":"platformio"}},"versions":[{{"name":"{framework_version}","files":[{{"system":"*","download_url":"{framework_url}","checksum":{{"sha256":"{framework_sha}"}}}}]}}]}}"#
        );
        let framework_payload = resolve_registry_json(
            framework.spec.registry().unwrap(),
            framework.kind,
            "linux_x86_64",
            &framework_metadata,
        )
        .unwrap();
        assert_eq!(framework_payload.url, framework_url);
        assert_eq!(framework_payload.sha256, framework_sha);
        assert_ne!(
            framework_payload.cache_identity(),
            platform_payload.cache_identity()
        );
        let toolchain = require_platform_package(&requirements, "toolchain-atmelavr").unwrap();
        assert_eq!(
            toolchain.spec.registry().unwrap().requirement.as_deref(),
            Some(toolchain_range)
        );
        let toolchain_url = "https://dl.registry.platformio.org/download/platformio/tool/toolchain-atmelavr/1.70300.191015/toolchain-atmelavr-linux_x86_64-1.70300.191015.tar.gz";
        let toolchain_sha = "664f0b7b08a15e8d6362b87e2fdb5a4dcfd5959ebc5b19d938e7dff6f12f0524";
        let toolchain_metadata = format!(
            r#"{{"name":"toolchain-atmelavr","owner":{{"username":"platformio"}},"versions":[{{"name":"1.70300.191015","files":[{{"system":"linux_x86_64","download_url":"{toolchain_url}","checksum":{{"sha256":"{toolchain_sha}"}}}}]}}]}}"#
        );
        let toolchain_payload = resolve_registry_json(
            toolchain.spec.registry().unwrap(),
            toolchain.kind,
            "linux_x86_64",
            &toolchain_metadata,
        )
        .unwrap();
        assert_eq!(toolchain_payload.version, "1.70300.191015");
        assert_eq!(toolchain_payload.url, toolchain_url);
        assert_eq!(toolchain_payload.sha256, toolchain_sha);
        assert_ne!(
            toolchain_payload.cache_identity(),
            framework_payload.cache_identity()
        );
    }
}

#[test]
fn unavailable_exact_minicore_pin_does_not_fall_back_to_older_core() {
    let metadata = r#"{"name":"framework-arduino-avr-minicore","owner":{"username":"platformio"},"versions":[{"name":"3.1.2","files":[{"system":"*","download_url":"https://dl.registry.platformio.org/download/platformio/tool/framework-arduino-avr-minicore/3.1.2/framework-arduino-avr-minicore-3.1.2.tar.gz","checksum":{"sha256":"3cc43553f35d2d00a277d488eefba628816104ae61a9ca9cecd60470f555c9f6"}}]},{"name":"2.2.2","files":[{"system":"*","download_url":"https://dl.registry.platformio.org/download/platformio/tool/framework-arduino-avr-minicore/2.2.2/framework-arduino-avr-minicore-2.2.2.tar.gz","checksum":{"sha256":"7172d1b1977237bcb996b61c12770ac3cc16ea422f0d44b2856b2d3a322c752e"}}]}]}"#;
    let unavailable = parse_package_spec("framework-arduino-avr-minicore@3.1.0").unwrap();
    assert!(
        resolve_registry_json(
            unavailable.registry().unwrap(),
            PackageKind::Framework,
            "linux_x86_64",
            metadata
        )
        .is_err()
    );
}
