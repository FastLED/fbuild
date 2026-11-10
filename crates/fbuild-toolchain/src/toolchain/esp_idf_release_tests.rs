//! Unit + end-to-end proofs for the pinned ESP-IDF tool components (#1970).
//!
//! Split out of `esp_idf_release.rs` to keep that module under the repository's
//! 1000-LOC ceiling; the tests are a child module of it, so `super::*` reaches
//! the private root rules as well as the public pins.

use super::*;

fn linux() -> HostPlatform {
    HostPlatform::new(HostOs::Linux, HostArch::X86_64)
}

#[test]
fn s3_profile_installs_exactly_the_xtensa_side_components() {
    let ids: Vec<&str> = IdfProfile::Esp32S3Only
        .components()
        .iter()
        .filter(|c| !c.is_build_tool())
        .map(|c| c.id())
        .collect();
    assert_eq!(
        ids,
        [
            "xtensa-esp-elf",
            "esp32ulp-elf",
            "esp-rom-elfs",
            "openocd-esp32"
        ]
    );
}

#[test]
fn s3_profile_installs_the_two_host_build_tools_after_the_toolchains() {
    let all: Vec<&str> = IdfProfile::Esp32S3Only
        .components()
        .iter()
        .map(|c| c.id())
        .collect();
    assert_eq!(
        all,
        [
            "xtensa-esp-elf",
            "esp32ulp-elf",
            "esp-rom-elfs",
            "openocd-esp32",
            "cmake",
            "ninja"
        ]
    );
    // `idf.py build` dead-ends without either one: CMake configures,
    // Ninja builds. Both are `on_request` upstream, i.e. a host
    // dependency, which is what a self-contained profile must own.
    for tool in IdfComponent::BUILD_TOOLS {
        assert!(tool.is_build_tool());
        assert!(IdfProfile::Esp32S3Only.components().contains(&tool));
    }
    assert_eq!(
        IdfComponent::BUILD_TOOLS.map(IdfComponent::id),
        ["cmake", "ninja"]
    );
}

#[test]
fn the_other_targets_toolchains_are_not_pinned_at_all() {
    // Not "filtered out at install time": absent from the pin table, so no
    // code path can install 2.1 GiB of RISC-V bits into an S3 profile.
    for excluded in [
        "riscv32-esp-elf",
        "riscv32-esp-elf-gdb",
        "xtensa-esp-elf-gdb",
    ] {
        assert!(
            !PINS.iter().any(|p| p.component == excluded),
            "{excluded} must not be pinned"
        );
        assert!(
            S3_EXCLUDED.iter().any(|(name, _)| *name == excluded)
                || excluded == "riscv32-esp-elf-gdb"
        );
    }
}

#[test]
fn every_s3_component_pins_a_verifiable_linux_archive() {
    for component in IdfProfile::Esp32S3Only.components() {
        let pin = pin(*component, linux()).unwrap();
        assert_eq!(pin.component, component.id());
        assert_eq!(
            pin.sha256.len(),
            64,
            "{} needs a full sha256",
            pin.component
        );
        assert!(
            pin.sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c.is_ascii_uppercase()),
            "{} sha256 is not hex",
            pin.component
        );
        // Espressif publishes its own toolchains; the host build tools come
        // from their upstream projects (Kitware, ninja-build), exactly as
        // `tools.json` points at them.
        assert!(
            pin.url.starts_with("https://github.com/espressif/")
                || pin.url.starts_with("https://github.com/Kitware/CMake/")
                || pin.url.starts_with("https://github.com/ninja-build/ninja/"),
            "{} must come from a pinned upstream release: {}",
            pin.component,
            pin.url
        );
        // Ninja's single-file zip is ~123 KB, so a megabyte floor would
        // reject a perfectly good pin.
        assert!(pin.bytes > 100_000, "{} pin has no size", pin.component);
        assert!(!pin.version.is_empty());
    }
}

#[test]
fn platform_keys_match_tools_json_spellings() {
    assert_eq!(platform_key(linux()), Some("linux-amd64"));
    assert_eq!(
        platform_key(HostPlatform::new(HostOs::Linux, HostArch::Aarch64)),
        Some("linux-arm64")
    );
    assert_eq!(
        platform_key(HostPlatform::new(HostOs::Macos, HostArch::Aarch64)),
        Some("macos-arm64")
    );
    assert_eq!(
        platform_key(HostPlatform::new(HostOs::Macos, HostArch::X86_64)),
        Some("macos")
    );
    assert_eq!(
        platform_key(HostPlatform::new(HostOs::Windows, HostArch::X86_64)),
        Some("win32")
    );
    // Unpinned hosts must error rather than borrow another host's binaries.
    assert_eq!(
        platform_key(HostPlatform::new(HostOs::Linux, HostArch::Arm)),
        None
    );
    assert_eq!(
        platform_key(HostPlatform::new(HostOs::Linux, HostArch::X86)),
        None
    );
    assert!(
        pin(
            IdfComponent::XtensaEspElf,
            HostPlatform::new(HostOs::Linux, HostArch::Arm)
        )
        .is_err()
    );
}

#[test]
fn esp_rom_elfs_falls_back_to_the_host_independent_archive() {
    let pin = pin(
        IdfComponent::EspRomElfs,
        HostPlatform::new(HostOs::Windows, HostArch::X86_64),
    )
    .unwrap();
    assert_eq!(pin.platform, "any");
}

#[test]
fn two_idf_releases_install_side_by_side() {
    let cache = tempfile::TempDir::new().unwrap();
    let v551 = EspIdfTool::with_cache_root(
        cache.path(),
        cache.path(),
        IdfComponent::XtensaEspElf,
        linux(),
    )
    .unwrap();
    let other = PackageBase::with_cache_root(
        "esp-idf-v5.4.0-xtensa-esp-elf",
        "v5.4.0+esp-14.2.0_20241119",
        "https://example.invalid/xtensa.tar.xz",
        "esp-idf-v5.4.0/xtensa-esp-elf",
        None,
        CacheSubdir::Toolchains,
        cache.path(),
        cache.path(),
    );
    assert_ne!(v551.install_path().as_path(), other.install_path());
    assert_eq!(
        v551.install_path()
            .as_path()
            .file_name()
            .and_then(|n| n.to_str()),
        Some("v5.5.1+esp-14.2.0_20241119"),
        "install leaf must name the IDF release: {}",
        v551.install_path()
    );
}

#[test]
fn s3_profile_measured_size_fits_the_budget() {
    let toolchains = IdfProfile::Esp32S3Only.measured_installed_bytes();
    // AC2 of #1970: <= 1.3 GB against `du`, not against an estimate.
    assert!(
        toolchains <= 1_300_000_000,
        "S3 profile measured {toolchains} B"
    );
    assert!(
        toolchains >= 1_100_000_000,
        "measurement looks truncated: {toolchains} B"
    );
    // The host build tools are a separate, much smaller budget: they are
    // host programs, not the cross toolchains AC2 measured.
    let build_tools = IdfProfile::Esp32S3Only.measured_build_tools_bytes();
    assert_eq!(
        build_tools,
        BUILD_TOOLS_MEASURED_INSTALLED_BYTES
            .iter()
            .map(|(_, b)| b)
            .sum::<u64>()
    );
    assert!(
        build_tools <= 200_000_000,
        "build tools measured {build_tools} B"
    );
    // CMake and Ninja are what push the whole tool set past AC2's 1.3 GB,
    // which is why they are budgeted separately instead of hidden inside it.
    assert!(toolchains + build_tools <= 1_350_000_000);
}

#[test]
fn tool_root_finds_the_nested_export_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let nested = tmp
        .path()
        .join("xtensa-esp-elf-14.2.0")
        .join("xtensa-esp-elf")
        .join("bin");
    std::fs::create_dir_all(&nested).unwrap();
    let root = tool_root(tmp.path(), IdfComponent::XtensaEspElf).unwrap();
    assert!(root.as_path().ends_with("xtensa-esp-elf"));
    assert!(tool_root(tmp.path(), IdfComponent::Esp32UlpElf).is_err());
}

#[test]
fn cmake_root_is_the_container_that_owns_bin_cmake() {
    // The container names its own release, so it cannot be a constant.
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = tmp.path().join("cmake-3.30.2-linux-x86_64").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join(fbuild_core::platform::executable::native_name("cmake"));
    std::fs::write(&exe, b"#!cmake").unwrap();
    let root = tool_root(tmp.path(), IdfComponent::Cmake).unwrap();
    assert!(
        root.as_path().file_name().and_then(|n| n.to_str()) == Some("cmake-3.30.2-linux-x86_64"),
        "expected the container, got {root}"
    );
    // Which is what makes `root/bin` a PATH entry the build can use.
    assert!(root.join("bin").as_path().is_dir());
}

#[test]
fn cmake_root_follows_the_macos_app_bundle() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = tmp
        .path()
        .join("cmake-3.30.2-macos-universal")
        .join("CMake.app")
        .join("Contents")
        .join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        bin.join(fbuild_core::platform::executable::native_name("cmake")),
        b"#",
    )
    .unwrap();
    let root = tool_root(tmp.path(), IdfComponent::Cmake).unwrap();
    // The owning directory found in a bundle is `CMake.app/Contents`, three
    // levels below the archive root -- and `root/bin` is still the PATH
    // entry, which is the only thing a consumer of the root asks for.
    assert_eq!(
        root,
        NormalizedPath::new(tmp.path())
            .join("cmake-3.30.2-macos-universal")
            .join("CMake.app")
            .join("Contents")
    );
    assert!(
        root.join("bin")
            .join(fbuild_core::platform::executable::native_name("cmake"))
            .as_path()
            .is_file()
    );
}

#[test]
fn ninja_root_is_the_archive_root_and_must_hold_the_binary() {
    let tmp = tempfile::TempDir::new().unwrap();
    let exe = tmp
        .path()
        .join(fbuild_core::platform::executable::native_name("ninja"));
    // An extraction that produced an empty directory is not an install.
    assert!(tool_root(tmp.path(), IdfComponent::Ninja).is_err());
    std::fs::write(&exe, b"#").unwrap();
    let root = tool_root(tmp.path(), IdfComponent::Ninja).unwrap();
    assert_eq!(root, NormalizedPath::new(tmp.path()));
}

/// Real download + sha256 verify + extract, through the shipped code path.
/// Ignored by default (needs network, ~3.5 MB); run with
/// `soldr cargo test -p fbuild-toolchain --lib -- --ignored esp_rom_elfs`.
#[tokio::test]
#[ignore = "downloads the pinned esp-rom-elfs archive"]
async fn esp_rom_elfs_installs_from_the_pinned_archive() {
    let dir = tempfile::TempDir::new().unwrap();
    let tool =
        EspIdfTool::with_cache_root(dir.path(), dir.path(), IdfComponent::EspRomElfs, linux())
            .unwrap();
    assert!(!tool.is_installed());
    let root = tool.ensure_normalized().await.unwrap();
    assert!(tool.is_installed());
    // The archive is flat: the ROM ELFs land directly in the tool root.
    assert!(
        root.join("esp32s3_rev0_rom.elf").as_path().is_file(),
        "expected the extracted ROM ELFs in {root}"
    );
    // Second call is a cache hit: no re-download, same tree.
    let again = tool.ensure_normalized().await.unwrap();
    assert_eq!(root, again);
}
