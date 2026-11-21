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
fn the_s3_profile_never_installs_another_targets_toolchain() {
    // The RISC-V compiler is pinned for `IdfProfile::Riscv32` (#1878's C5/P4
    // gap), but it is 2.1 GiB an S3 build never runs, so it must stay out of
    // the S3 component list rather than be filtered at install time.
    assert!(
        !IdfProfile::Esp32S3Only
            .components()
            .contains(&IdfComponent::Riscv32EspElf)
    );
    assert!(
        S3_EXCLUDED
            .iter()
            .any(|(name, _)| *name == "riscv32-esp-elf")
    );
    // The debuggers are not pinned at all: no profile can install them.
    for excluded in ["riscv32-esp-elf-gdb", "xtensa-esp-elf-gdb"] {
        assert!(
            !PINS.iter().any(|p| p.component == excluded),
            "{excluded} must not be pinned"
        );
    }
}

#[test]
fn riscv_profile_installs_the_riscv_toolchain_and_nothing_xtensa() {
    let all: Vec<&str> = IdfProfile::Riscv32
        .components()
        .iter()
        .map(|c| c.id())
        .collect();
    // `tools.json` scopes `xtensa-esp-elf` and `esp32ulp-elf` to
    // esp32/esp32s2/esp32s3; a RISC-V target's LP core is built by the same
    // riscv32 compiler, so neither belongs here.
    assert_eq!(
        all,
        [
            "riscv32-esp-elf",
            "esp-rom-elfs",
            "openocd-esp32",
            "cmake",
            "ninja"
        ]
    );
    for tool in IdfComponent::BUILD_TOOLS {
        assert!(IdfProfile::Riscv32.components().contains(&tool));
    }
}

#[test]
fn riscv_toolchain_is_pinned_for_every_supported_host() {
    let hosts = [
        (HostOs::Linux, HostArch::X86_64, "linux-amd64"),
        (HostOs::Linux, HostArch::Aarch64, "linux-arm64"),
        (HostOs::Macos, HostArch::X86_64, "macos"),
        (HostOs::Macos, HostArch::Aarch64, "macos-arm64"),
        (HostOs::Windows, HostArch::X86_64, "win32"),
    ];
    for (os, arch, key) in hosts {
        let pin = pin(IdfComponent::Riscv32EspElf, HostPlatform::new(os, arch)).unwrap();
        assert_eq!(pin.component, "riscv32-esp-elf");
        assert_eq!(pin.platform, key);
        // The same crosstool-NG release as the Xtensa compiler: one IDF
        // release recommends one compiler set.
        assert_eq!(pin.version, "esp-14.2.0_20241119");
        assert_eq!(pin.sha256.len(), 64, "{key} needs a full sha256");
        assert!(pin.sha256.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(
            pin.url.starts_with(
                "https://github.com/espressif/crosstool-NG/releases/download/esp-14.2.0_20241119/riscv32-esp-elf-"
            ),
            "{}",
            pin.url
        );
        assert!(pin.bytes > 200_000_000, "{key} pin has no size");
    }
    // Unpinned hosts still error rather than borrow another host's binaries.
    assert!(
        pin(
            IdfComponent::Riscv32EspElf,
            HostPlatform::new(HostOs::Linux, HostArch::Arm)
        )
        .is_err()
    );
}

#[test]
fn profiles_share_one_release_and_the_components_they_have_in_common() {
    // Both profiles install the same IDF release, so a component they share
    // (ROM ELFs, OpenOCD, CMake, Ninja) is one cache entry, not one per profile.
    assert_eq!(
        IdfProfile::Riscv32.cache_prefix(),
        IdfProfile::Esp32S3Only.cache_prefix()
    );
    let shared: Vec<IdfComponent> = IdfProfile::Riscv32
        .components()
        .iter()
        .copied()
        .filter(|c| IdfProfile::Esp32S3Only.components().contains(c))
        .collect();
    assert_eq!(
        shared,
        [
            IdfComponent::EspRomElfs,
            IdfComponent::OpenocdEsp32,
            IdfComponent::Cmake,
            IdfComponent::Ninja
        ]
    );
}

#[test]
fn riscv_profile_measured_size_is_reported_not_hidden() {
    // The RISC-V compiler alone is ~2.1 GB installed: almost twice the S3
    // profile, which is why a chip gets it only by asking for it.
    let toolchains = IdfProfile::Riscv32.measured_installed_bytes();
    assert!(
        (2_100_000_000..=2_300_000_000).contains(&toolchains),
        "RISC-V profile measured {toolchains} B"
    );
    assert_eq!(
        IdfProfile::Riscv32.measured_build_tools_bytes(),
        IdfProfile::Esp32S3Only.measured_build_tools_bytes()
    );
}

#[test]
fn riscv_tool_root_finds_the_nested_export_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let nested = tmp
        .path()
        .join("riscv32-esp-elf-14.2.0")
        .join("riscv32-esp-elf")
        .join("bin");
    std::fs::create_dir_all(&nested).unwrap();
    let root = tool_root(tmp.path(), IdfComponent::Riscv32EspElf).unwrap();
    assert!(root.as_path().ends_with("riscv32-esp-elf"));
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
