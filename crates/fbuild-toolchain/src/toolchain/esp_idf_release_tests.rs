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

/// The pinned release's own `tools.json`, committed verbatim beside the pins.
///
/// `PINS` is a hand-copied projection of this file. The tests below compare the
/// two row by row, so moving the pin (#2133) is a data diff a reviewer can read
/// instead of a transcription exercise nothing can check.
const RELEASE_TOOLS_JSON: &str = include_str!("../../data/esp-idf-v5.5.1-tools.json");

/// The release the snapshot above was taken from, spelled as its file name is.
/// Kept as its own const so a version bump that forgets the snapshot fails
/// `the_tools_json_snapshot_belongs_to_the_pinned_release` instead of silently
/// checking v5.5.1 data against a newer table.
const SNAPSHOT_RELEASE: &str = "v5.5.1";

/// Upstream `tools.json` platform spellings that differ from fbuild's key for
/// the same host. `ninja` is the one tool Espressif publishes under `win64`.
const PLATFORM_ALIASES: &[(&str, &str)] = &[("win32", "win64")];

/// The five hosts `platform_key` supports, as real `HostPlatform`s.
fn supported_hosts() -> [HostPlatform; 5] {
    [
        HostPlatform::new(HostOs::Linux, HostArch::X86_64),
        HostPlatform::new(HostOs::Linux, HostArch::Aarch64),
        HostPlatform::new(HostOs::Macos, HostArch::X86_64),
        HostPlatform::new(HostOs::Macos, HostArch::Aarch64),
        HostPlatform::new(HostOs::Windows, HostArch::X86_64),
    ]
}

/// Every component a delegated ESP-IDF build can ask for, across both profiles.
fn delegated_components() -> Vec<IdfComponent> {
    let mut all: Vec<IdfComponent> = Vec::new();
    for profile in [IdfProfile::Esp32S3Only, IdfProfile::Riscv32] {
        for component in profile.components() {
            if !all.contains(component) {
                all.push(*component);
            }
        }
    }
    all
}

/// The recommended version of `component` that the pins use for `host`.
fn recommended_version_for<'a>(
    json: &'a serde_json::Value,
    component: &str,
    host: HostPlatform,
) -> Option<&'a serde_json::Value> {
    let key = platform_key(host)?;
    let pinned = PINS
        .iter()
        .find(|p| p.component == component && (p.platform == key || p.platform == "any"))?;
    recommended_version(json, component, pinned.version)
}

fn release_tools_json() -> serde_json::Value {
    serde_json::from_str(RELEASE_TOOLS_JSON)
        .expect("the pinned tools.json snapshot must be valid JSON")
}

/// The `status: recommended` entry for `component` at `version`, or `None`.
fn recommended_version<'a>(
    json: &'a serde_json::Value,
    component: &str,
    version: &str,
) -> Option<&'a serde_json::Value> {
    let tool = json["tools"]
        .as_array()?
        .iter()
        .find(|t| t["name"].as_str() == Some(component))?;
    tool["versions"].as_array()?.iter().find(|v| {
        v["name"].as_str() == Some(version) && v["status"].as_str() == Some("recommended")
    })
}

/// One host's archive inside a recommended version, following the alias table.
fn archive_for<'a>(
    version: &'a serde_json::Value,
    platform: &str,
) -> Option<&'a serde_json::Value> {
    let alias = PLATFORM_ALIASES
        .iter()
        .find(|(ours, _)| *ours == platform)
        .map(|(_, upstream)| *upstream);
    version
        .get(platform)
        .or_else(|| alias.and_then(|key| version.get(key)))
}

#[test]
fn the_tools_json_snapshot_belongs_to_the_pinned_release() {
    assert_eq!(
        SNAPSHOT_RELEASE, IDF_RELEASE_VERSION,
        "data/esp-idf-{SNAPSHOT_RELEASE}-tools.json is checked against the pins, but the \
         delegated pin is {IDF_RELEASE_VERSION}: replace the snapshot when the pin moves"
    );
}

#[test]
fn pins_are_verbatim_rows_of_the_release_tools_json() {
    let json = release_tools_json();
    let mut mismatches = Vec::new();
    for pin in PINS {
        let Some(version) = recommended_version(&json, pin.component, pin.version) else {
            mismatches.push(format!(
                "{}: no `recommended` {} entry in the pinned release's tools.json",
                pin.component, pin.version
            ));
            continue;
        };
        let Some(archive) = archive_for(version, pin.platform) else {
            mismatches.push(format!(
                "{} {}: tools.json has no `{}` archive",
                pin.component, pin.version, pin.platform
            ));
            continue;
        };
        if archive["sha256"].as_str() != Some(pin.sha256) {
            mismatches.push(format!(
                "{} {}: sha256 is {} upstream, {} here",
                pin.component,
                pin.platform,
                archive["sha256"].as_str().unwrap_or("<none>"),
                pin.sha256
            ));
        }
        if archive["size"].as_u64() != Some(pin.bytes) {
            mismatches.push(format!(
                "{} {}: size is {} upstream, {} here",
                pin.component, pin.platform, archive["size"], pin.bytes
            ));
        }
        if archive["url"].as_str() != Some(pin.url) {
            mismatches.push(format!(
                "{} {}: url is {} upstream, {} here",
                pin.component,
                pin.platform,
                archive["url"].as_str().unwrap_or("<none>"),
                pin.url
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} pin row(s) disagree with data/esp-idf-{SNAPSHOT_RELEASE}-tools.json:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn pins_cover_exactly_the_archives_upstream_publishes() {
    // Coverage, both ways: every archive the pinned release recommends for a
    // host fbuild supports must have a `PINS` row, and `PINS` may not invent a
    // row for a host upstream publishes nothing for (which would silently
    // install another host's binaries).
    let json = release_tools_json();
    let mut missing = Vec::new();
    let mut invented = Vec::new();
    let mut reached: Vec<(&str, &str)> = Vec::new();
    for component in delegated_components() {
        for host in supported_hosts() {
            let key = platform_key(host).expect("supported host");
            let version = recommended_version_for(&json, component.id(), linux())
                .unwrap_or_else(|| panic!("{} is not a recommended release", component.id()));
            // `esp-rom-elfs` publishes one host-independent archive under `any`.
            let upstream = archive_for(version, key)
                .or_else(|| version.get("any"))
                .is_some();
            match (upstream, pin(component, host)) {
                (true, Ok(pinned)) => {
                    assert!(
                        pinned.platform == key || pinned.platform == "any",
                        "{} {key} borrowed {}",
                        component.id(),
                        pinned.platform
                    );
                    reached.push((pinned.component, pinned.platform));
                }
                (true, Err(e)) => missing.push(format!("{} {key}: {e}", component.id())),
                (false, Ok(_)) => invented.push(format!("{} {key}", component.id())),
                // Upstream publishes nothing for this host either: no pin is
                // the honest answer, and `pin` above must refuse it.
                (false, Err(_)) => {}
            }
        }
    }
    assert!(
        missing.is_empty() && invented.is_empty(),
        "pins disagree with the release's host coverage:\n  missing:\n{}\n  invented:\n{}",
        missing.join("\n  "),
        invented.join("\n  ")
    );
    // Rows, not (row, host) pairs: `esp-rom-elfs`'s single `any` row answers for
    // all five hosts. This is the dead-row check — a pin nothing can reach is a
    // typo in `platform`, not coverage.
    reached.sort_unstable();
    reached.dedup();
    let mut pinned_rows: Vec<(&str, &str)> =
        PINS.iter().map(|p| (p.component, p.platform)).collect();
    pinned_rows.sort_unstable();
    assert_eq!(
        reached, pinned_rows,
        "every pin must be reachable from a supported host"
    );
}

#[test]
fn upstream_publishes_no_linux_arm64_ninja_so_a_delegated_build_there_has_none() {
    // Recorded, not papered over: `tools.json` for the pinned release has no
    // `linux-arm64` ninja archive (the five keys are linux-amd64, macos,
    // macos-arm64 and win64), so a delegated `idf.py` build on an arm64 Linux
    // host cannot get a pinned ninja even though every other component is
    // pinned there. If upstream ever publishes one this test fails and the
    // pin should be added; until then `pin()` must refuse rather than borrow
    // another host's binary.
    let json = release_tools_json();
    let version = recommended_version(&json, "ninja", "1.12.1").expect("ninja 1.12.1");
    assert!(
        archive_for(version, "linux-arm64").is_none(),
        "upstream now publishes a linux-arm64 ninja: add the pin"
    );
    assert!(
        pin(
            IdfComponent::Ninja,
            HostPlatform::new(HostOs::Linux, HostArch::Aarch64)
        )
        .is_err(),
        "no pinned ninja exists for linux-arm64, so the lookup must refuse"
    );
}

#[test]
fn the_release_publishes_one_recommended_version_per_pinned_component() {
    let json = release_tools_json();
    for component in IdfProfile::Esp32S3Only.components() {
        let versions: Vec<String> = json["tools"]
            .as_array()
            .expect("tools[]")
            .iter()
            .filter(|t| t["name"].as_str() == Some(component.id()))
            .flat_map(|t| t["versions"].as_array().cloned().unwrap_or_default())
            .filter(|v| v["status"].as_str() == Some("recommended"))
            .map(|v| v["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            versions.len(),
            1,
            "{} has {} `recommended` versions in tools.json, so a pin cannot name one \
             release and install another",
            component.id(),
            versions.len()
        );
        let pinned = pin(*component, linux()).unwrap();
        assert_eq!(
            versions[0],
            pinned.version,
            "{} pins {} but tools.json recommends {}",
            component.id(),
            pinned.version,
            versions[0]
        );
    }
}
