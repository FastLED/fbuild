//! Offline CH32V VCS platform/package identity fixture.

use fbuild_core::platformio_package::{
    PackageKind, PackageLock, PackageSource, parse_package_spec, require_platform_package,
    resolve_platform_requirements,
};

const MANIFEST: &str = r#"{
  "version": "1.1.0",
  "frameworks": {"arduino": {"package": "framework-arduinoch32v003"}},
  "packages": {
    "toolchain-riscv": {"type": "toolchain", "owner": "platformio", "version": "https://github.com/Community-PIO-CH32V/toolchain-riscv-windows.git"},
    "framework-arduino-openwch-ch32": {"type": "framework", "optional": true, "version": "https://github.com/Community-PIO-CH32V/arduino_core_ch32.git"}
  }
}"#;

#[test]
fn pinned_ch32v_platform_source_has_immutable_identity_and_selected_packages() {
    let platform = parse_package_spec("https://github.com/Community-PIO-CH32V/platform-ch32v.git#b7397c29a71101175bfc94f6ab06f9daac336458").unwrap();
    let PackageSource::Repository { url, reference } = platform.source else {
        panic!("CH32V platform must remain a VCS source");
    };
    let lock = PackageLock::Repository {
        url: url.clone(),
        commit: reference.unwrap(),
    };
    assert_eq!(lock.cache_identity().len(), 64);

    let linux_toolchain = parse_package_spec(
        "toolchain-riscv@https://github.com/Community-PIO-CH32V/toolchain-riscv-linux.git",
    )
    .unwrap();
    let requirements = resolve_platform_requirements(MANIFEST, &[linux_toolchain]).unwrap();
    let toolchain = require_platform_package(&requirements, "toolchain-riscv").unwrap();
    assert_eq!(toolchain.kind, PackageKind::Tool);
    assert!(matches!(
        &toolchain.spec.source,
        PackageSource::Repository { url, reference: None }
            if url.ends_with("toolchain-riscv-linux.git")
    ));
    let framework =
        require_platform_package(&requirements, "framework-arduino-openwch-ch32").unwrap();
    assert_eq!(framework.kind, PackageKind::Framework);
    assert!(matches!(
        &framework.spec.source,
        PackageSource::Repository { url, reference: None }
            if url.ends_with("arduino_core_ch32.git")
    ));
    assert_ne!(
        lock.cache_identity(),
        PackageLock::Repository {
            url,
            commit: "0000000000000000000000000000000000000000".into(),
        }
        .cache_identity()
    );
}

#[test]
fn explicit_ch32v_package_pin_precedes_selected_manifest_source() {
    let explicit = parse_package_spec("platformio/toolchain-riscv@1.2.3").unwrap();
    let requirements = resolve_platform_requirements(MANIFEST, &[explicit]).unwrap();
    let toolchain = require_platform_package(&requirements, "toolchain-riscv").unwrap();
    assert_eq!(
        toolchain.spec.registry().unwrap().requirement.as_deref(),
        Some("1.2.3")
    );
}
