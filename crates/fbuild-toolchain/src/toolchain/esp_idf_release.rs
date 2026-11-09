//! Pinned ESP-IDF release tool components, provisioned as fbuild packages.
//!
//! FastLED/fbuild#1970. The merged research pass
//! (`docs/design/bare-idf-component-builds.md`) measured IDF v5.5.1's own
//! bootstrap: `install.sh` needs a venv with a working `ensurepip` and, for
//! `openocd-esp32`, a loadable `libusb-1.0.so.0`, so it cannot provision a
//! toolchain on every host fbuild supports. This module pins the archives
//! Espressif publishes **for that release** instead — one fbuild package per
//! component — so a delegated `idf.py` build (#1971) resolves them from the
//! fbuild cache like any other toolchain, with no `install.sh` and no
//! `IDF_TOOLS_PATH` side tree.
//!
//! Pins are copied from the release's own `tools/tools.json` (`status:
//! recommended`), sha256 included: no archive is trusted by URL alone. The
//! `tools.json` platform set is a superset of what fbuild supports, so
//! `linux-armel`, `linux-armhf` and `linux-i686` are deliberately unpinned and
//! asking for one is an error, never a silent fallback to another host's bits.
//!
//! Scope: the **tool components** only. The IDF source checkout and its Python
//! environment are follow-up work (#1971 needs them to run `idf.py`); this
//! module is the part `install.sh` gets wrong and the part that costs 3.4 GB.

use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_core::platform::host::{self, HostArch, HostOs, HostPlatform};
use fbuild_core::{FbuildError, Result};

use crate::{CacheSubdir, PackageBase, PackageInfo};

/// The single ESP-IDF release this module pins.
pub const IDF_RELEASE_VERSION: &str = "v5.5.1";

/// Commit `v5.5.1` resolves to (`git rev-parse HEAD` in a v5.5.1 checkout).
pub const IDF_RELEASE_COMMIT: &str = "fcae32885b0296b32044cb99ecbdc50d98dddb83";

/// One tool component the ESP32-S3 profile installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdfComponent {
    /// GCC/binutils for ESP32, ESP32-S2 and ESP32-S3 (the only cross compiler an
    /// ESP32-S3 project needs).
    XtensaEspElf,
    /// Binutils for the ULP coprocessor.
    Esp32UlpElf,
    /// ROM ELF symbol files the default linker scripts reference.
    EspRomElfs,
    /// On-chip debugger; needed only for `idf.py openocd` / JTAG debugging.
    OpenocdEsp32,
}

impl IdfComponent {
    /// Every component [`IdfProfile::Esp32S3Only`] installs, in install order.
    pub const ALL: [Self; 4] = [
        Self::XtensaEspElf,
        Self::Esp32UlpElf,
        Self::EspRomElfs,
        Self::OpenocdEsp32,
    ];

    /// `tools.json` name — also the directory name the archive exports.
    pub const fn id(self) -> &'static str {
        match self {
            Self::XtensaEspElf => "xtensa-esp-elf",
            Self::Esp32UlpElf => "esp32ulp-elf",
            Self::EspRomElfs => "esp-rom-elfs",
            Self::OpenocdEsp32 => "openocd-esp32",
        }
    }

    /// Directory inside the extracted archive that is the tool root.
    /// Empty string means "the archive root itself".
    const fn export_dir(self) -> &'static str {
        match self {
            Self::EspRomElfs => "",
            Self::XtensaEspElf | Self::Esp32UlpElf | Self::OpenocdEsp32 => self.id(),
        }
    }
}

/// Components `install.sh` installs by default that the ESP32-S3 profile skips,
/// with the installed size measured on linux-amd64 (2026-11-09, `du -sb`).
pub const S3_EXCLUDED: &[(&str, u64)] = &[
    // RISC-V cross compiler: 2.1 GiB, used by C2/C3/C5/C6/H2/P4 only.
    ("riscv32-esp-elf", 2_142_092_152),
    // GDB for Xtensa: 104 MiB, debugging only.
    ("xtensa-esp-elf-gdb", 108_817_092),
];

/// Installed bytes of the ESP32-S3 profile, measured with `du -sb` on linux-amd64
/// (2026-11-09, ESP-IDF v5.5.1 as installed by `install.sh` into a scratch tree).
pub const S3_MEASURED_INSTALLED_BYTES: &[(&str, u64)] = &[
    ("xtensa-esp-elf", 1_103_937_348),
    ("esp32ulp-elf", 31_188_493),
    ("esp-rom-elfs", 6_918_628),
    ("openocd-esp32", 7_153_401),
];

/// Which components an install pulls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdfProfile {
    /// Xtensa + ULP + ROM ELFs + OpenOCD: everything an ESP32-S3 build needs and
    /// nothing for the other eight targets.
    Esp32S3Only,
}

impl IdfProfile {
    /// Components this profile installs.
    pub fn components(self) -> &'static [IdfComponent] {
        match self {
            Self::Esp32S3Only => &IdfComponent::ALL,
        }
    }

    /// Measured installed bytes of the profile (`S3_MEASURED_INSTALLED_BYTES`).
    pub fn measured_installed_bytes(self) -> u64 {
        match self {
            Self::Esp32S3Only => S3_MEASURED_INSTALLED_BYTES.iter().map(|(_, b)| b).sum(),
        }
    }

    /// Cache key prefix: one IDF release, installed side by side with any other
    /// (`#1843`/`#1844` rule — the release is the discriminator, never omitted).
    pub fn cache_prefix(self) -> String {
        format!("esp-idf-{}", IDF_RELEASE_VERSION)
    }
}

/// One archive pin, straight out of the release's `tools.json`.
#[derive(Debug, Clone, Copy)]
pub struct EspIdfPin {
    pub component: &'static str,
    pub platform: &'static str,
    pub version: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub url: &'static str,
}

/// Platform keys fbuild supports, mapped to the `tools.json` spelling.
pub fn platform_key(host: HostPlatform) -> Option<&'static str> {
    match (host.os(), host.arch()) {
        (HostOs::Linux, HostArch::X86_64) => Some("linux-amd64"),
        (HostOs::Linux, HostArch::Aarch64) => Some("linux-arm64"),
        (HostOs::Macos, HostArch::Aarch64) => Some("macos-arm64"),
        (HostOs::Macos, HostArch::X86_64) => Some("macos"),
        (HostOs::Windows, HostArch::X86_64) => Some("win32"),
        _ => None,
    }
}

/// The pin for `component` on `host`, or an error naming the unsupported host.
pub fn pin(component: IdfComponent, host: HostPlatform) -> Result<&'static EspIdfPin> {
    let key = platform_key(host).ok_or_else(|| unsupported_host(component, host))?;
    PINS.iter()
        .find(|p| p.component == component.id() && p.platform == key)
        // `esp-rom-elfs` publishes one host-independent archive under `any`.
        .or_else(|| {
            PINS.iter()
                .find(|p| p.component == component.id() && p.platform == "any")
        })
        .ok_or_else(|| unsupported_host(component, host))
}

fn unsupported_host(component: IdfComponent, host: HostPlatform) -> FbuildError {
    FbuildError::PackageError(format!(
        "no pinned ESP-IDF {} archive for {:?} {:?} (pinned: linux-amd64, linux-arm64, macos, macos-arm64, win32)",
        component.id(),
        host.os(),
        host.arch()
    ))
}

/// `tools.json` of ESP-IDF v5.5.1, `status: recommended`, verbatim.
const PINS: &[EspIdfPin] = &[
    EspIdfPin {
        component: "xtensa-esp-elf",
        platform: "linux-amd64",
        version: "esp-14.2.0_20241119",
        sha256: "e3e6dcf3d275c3c9ab0e4c8a9d93fd10e7efc035d435460576c9d95b4140c676",
        bytes: 174652716,
        url: "https://github.com/espressif/crosstool-NG/releases/download/esp-14.2.0_20241119/xtensa-esp-elf-14.2.0_20241119-x86_64-linux-gnu.tar.xz",
    },
    EspIdfPin {
        component: "xtensa-esp-elf",
        platform: "linux-arm64",
        version: "esp-14.2.0_20241119",
        sha256: "ac2b311dc0003386425086bfc813bf2aeb3cdf3b117845802df6ebef5f69955f",
        bytes: 176920772,
        url: "https://github.com/espressif/crosstool-NG/releases/download/esp-14.2.0_20241119/xtensa-esp-elf-14.2.0_20241119-aarch64-linux-gnu.tar.xz",
    },
    EspIdfPin {
        component: "xtensa-esp-elf",
        platform: "macos",
        version: "esp-14.2.0_20241119",
        sha256: "1ca7a93825d5b84f6547fa73e6174acadb9af5c877f365e50e176936784383ea",
        bytes: 182782108,
        url: "https://github.com/espressif/crosstool-NG/releases/download/esp-14.2.0_20241119/xtensa-esp-elf-14.2.0_20241119-x86_64-apple-darwin.tar.xz",
    },
    EspIdfPin {
        component: "xtensa-esp-elf",
        platform: "macos-arm64",
        version: "esp-14.2.0_20241119",
        sha256: "b9732bb3cdcd6a50420c25eb9a8d90eb1216a0a0a274c8ffd4c2037167b5081f",
        bytes: 167948176,
        url: "https://github.com/espressif/crosstool-NG/releases/download/esp-14.2.0_20241119/xtensa-esp-elf-14.2.0_20241119-aarch64-apple-darwin.tar.xz",
    },
    EspIdfPin {
        component: "xtensa-esp-elf",
        platform: "win32",
        version: "esp-14.2.0_20241119",
        sha256: "b30e450e0af279783c54a9ae77c3b367dd556b78eda930a92ec7b784a74c28c8",
        bytes: 382457717,
        url: "https://github.com/espressif/crosstool-NG/releases/download/esp-14.2.0_20241119/xtensa-esp-elf-14.2.0_20241119-i686-w64-mingw32.zip",
    },
    EspIdfPin {
        component: "esp32ulp-elf",
        platform: "linux-amd64",
        version: "2.38_20240113",
        sha256: "d13a808365b78465fa6591636dfbbb9604d9d15a397c3d9cd22626d54828ac2c",
        bytes: 11138071,
        url: "https://github.com/espressif/binutils-gdb/releases/download/esp32ulp-elf-2.38_20240113/esp32ulp-elf-2.38_20240113-linux-amd64.tar.gz",
    },
    EspIdfPin {
        component: "esp32ulp-elf",
        platform: "linux-arm64",
        version: "2.38_20240113",
        sha256: "ecce0788ce1000e5c669c5adaf2fd5bf7f9bf96dcdbd3555d1d9ce4dcb311038",
        bytes: 10471496,
        url: "https://github.com/espressif/binutils-gdb/releases/download/esp32ulp-elf-2.38_20240113/esp32ulp-elf-2.38_20240113-linux-arm64.tar.gz",
    },
    EspIdfPin {
        component: "esp32ulp-elf",
        platform: "macos",
        version: "2.38_20240113",
        sha256: "b2aeba8eaafdf156e9e30be928dde1f133b00eaf33802d96827ec544ac7c864c",
        bytes: 16886021,
        url: "https://github.com/espressif/binutils-gdb/releases/download/esp32ulp-elf-2.38_20240113/esp32ulp-elf-2.38_20240113-macos.tar.gz",
    },
    EspIdfPin {
        component: "esp32ulp-elf",
        platform: "macos-arm64",
        version: "2.38_20240113",
        sha256: "e3a4dfea043e2bce8cd00b3a0b260a59249fa61ca5931bf02f18a3d43c18deb4",
        bytes: 15843725,
        url: "https://github.com/espressif/binutils-gdb/releases/download/esp32ulp-elf-2.38_20240113/esp32ulp-elf-2.38_20240113-macos-arm64.tar.gz",
    },
    EspIdfPin {
        component: "esp32ulp-elf",
        platform: "win32",
        version: "2.38_20240113",
        sha256: "d33b64f49df27dcfa4a24d3af1a5ead77b020f85f33448994c31b98f88e66bb4",
        bytes: 15421659,
        url: "https://github.com/espressif/binutils-gdb/releases/download/esp32ulp-elf-2.38_20240113/esp32ulp-elf-2.38_20240113-win32.zip",
    },
    EspIdfPin {
        component: "esp-rom-elfs",
        platform: "any",
        version: "20241011",
        sha256: "921f000164a421c7628fbfee55b173384aafaa51883adc65cd27bf9b0af9e9a9",
        bytes: 3540038,
        url: "https://github.com/espressif/esp-rom-elfs/releases/download/20241011/esp-rom-elfs-20241011.tar.gz",
    },
    EspIdfPin {
        component: "openocd-esp32",
        platform: "linux-amd64",
        version: "v0.12.0-esp32-20250707",
        sha256: "766293bd7a08900d3536f87a0a7ade960f07266f16e4147f95ca5ce4e15d4c5d",
        bytes: 2489724,
        url: "https://github.com/espressif/openocd-esp32/releases/download/v0.12.0-esp32-20250707/openocd-esp32-linux-amd64-0.12.0-esp32-20250707.tar.gz",
    },
    EspIdfPin {
        component: "openocd-esp32",
        platform: "linux-arm64",
        version: "v0.12.0-esp32-20250707",
        sha256: "34b6883c372444b49950893b2fc0101aefd10d404a88ef72c97e80199f8544d3",
        bytes: 2371243,
        url: "https://github.com/espressif/openocd-esp32/releases/download/v0.12.0-esp32-20250707/openocd-esp32-linux-arm64-0.12.0-esp32-20250707.tar.gz",
    },
    EspIdfPin {
        component: "openocd-esp32",
        platform: "macos",
        version: "v0.12.0-esp32-20250707",
        sha256: "6267be53892a76d535938a1b044b685adc7d292f090447e8a3e3d0f0996474d1",
        bytes: 2585348,
        url: "https://github.com/espressif/openocd-esp32/releases/download/v0.12.0-esp32-20250707/openocd-esp32-macos-0.12.0-esp32-20250707.tar.gz",
    },
    EspIdfPin {
        component: "openocd-esp32",
        platform: "macos-arm64",
        version: "v0.12.0-esp32-20250707",
        sha256: "150e938ac48a6ee031ddbc8b31043bc7f2073ab2ee4896b658918d35899673c3",
        bytes: 2628741,
        url: "https://github.com/espressif/openocd-esp32/releases/download/v0.12.0-esp32-20250707/openocd-esp32-macos-arm64-0.12.0-esp32-20250707.tar.gz",
    },
    EspIdfPin {
        component: "openocd-esp32",
        platform: "win32",
        version: "v0.12.0-esp32-20250707",
        sha256: "666274b04af7f36b430b6d063006051c37b8635b5175735ad5af07a1fbc6f486",
        bytes: 3034680,
        url: "https://github.com/espressif/openocd-esp32/releases/download/v0.12.0-esp32-20250707/openocd-esp32-win32-0.12.0-esp32-20250707.zip",
    },
];

/// An installable ESP-IDF tool component.
pub struct EspIdfTool {
    base: PackageBase,
    component: IdfComponent,
    export_dir: &'static str,
}

impl EspIdfTool {
    /// Resolve the pinned archive for `component` on the current host.
    pub fn new(project_dir: &Path, component: IdfComponent) -> Result<Self> {
        Self::build(project_dir, component, host::current(), None)
    }

    /// Install into an explicit cache root (tests and the end-to-end proof).
    #[cfg(test)]
    fn with_cache_root(
        project_dir: &Path,
        cache_root: &Path,
        component: IdfComponent,
        host: HostPlatform,
    ) -> Result<Self> {
        Self::build(project_dir, component, host, Some(cache_root))
    }

    fn build(
        project_dir: &Path,
        component: IdfComponent,
        host: HostPlatform,
        cache_root: Option<&Path>,
    ) -> Result<Self> {
        let pin = pin(component, host)?;
        // The IDF release is part of the cache key, so v5.5.1 and any later pin
        // install side by side instead of overwriting one another (#1843).
        let cache_key = format!(
            "{}/{}",
            IdfProfile::Esp32S3Only.cache_prefix(),
            component.id()
        );
        let name = format!("esp-idf-{}-{}", IDF_RELEASE_VERSION, component.id());
        // `<idf release>+<component version>`: the leaf names both, so two IDF
        // releases are readable in the tree and never share a path.
        let version = format!("{}+{}", IDF_RELEASE_VERSION, pin.version);
        let base = match cache_root {
            Some(root) => PackageBase::with_cache_root(
                &name,
                &version,
                pin.url,
                &cache_key,
                Some(pin.sha256),
                CacheSubdir::Toolchains,
                project_dir,
                root,
            ),
            None => PackageBase::new(
                &name,
                &version,
                pin.url,
                &cache_key,
                Some(pin.sha256),
                CacheSubdir::Toolchains,
                project_dir,
            ),
        };
        Ok(Self {
            base,
            component,
            export_dir: component.export_dir(),
        })
    }

    /// The pinned component.
    pub fn component(&self) -> IdfComponent {
        self.component
    }

    /// Where the component is (or would be) installed.
    pub fn install_path(&self) -> NormalizedPath {
        NormalizedPath::new(self.base.install_path())
    }

    /// True only when the archive is extracted **and** exports the directory
    /// `tools.json` names: a half-extracted tree is not an install, and a
    /// cache hit that lost the export dir must be repaired, not trusted.
    pub fn is_installed(&self) -> bool {
        self.base.is_cached() && tool_root(&self.base.install_path(), self.export_dir).is_ok()
    }

    /// Install the pinned archive and return the component's `export` root.
    ///
    /// Inherent, not a `Package` impl: `Package::ensure_installed` is typed
    /// `std::path::PathBuf`, and `dylints/ban_std_pathbuf` holds that legacy
    /// signature on a shrink-only allowlist of pre-existing files. New code
    /// returns [`NormalizedPath`]; a consumer that needs the trait can convert
    /// at its own legacy boundary (see `MarkerArchivePackage`).
    pub async fn ensure_normalized(&self) -> Result<NormalizedPath> {
        let export_dir = self.export_dir;
        if self.is_installed() {
            return tool_root(&self.base.install_path(), export_dir);
        }
        let installed = self
            .base
            .staged_install(move |root: &Path| tool_root(root, export_dir).map(|_| ()))
            .await?;
        tool_root(&installed, export_dir)
    }

    /// Package metadata for install reports and logging.
    pub fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

/// The tool root inside an extracted archive: the export dir at the root or one
/// level down (Espressif tarballs nest as `<name>-<version>/<name>/`).
///
/// Returns the error, rather than a path that may not exist, so the caller
/// cannot install "successfully" into a directory the release never produced.
fn tool_root(root: &Path, export_dir: &str) -> Result<NormalizedPath> {
    let root = NormalizedPath::new(root);
    if export_dir.is_empty() {
        return Ok(root);
    }
    let direct = root.join(export_dir);
    if direct.as_path().is_dir() {
        return Ok(direct);
    }
    if let Ok(entries) = std::fs::read_dir(root.as_path()) {
        for entry in entries.flatten() {
            let nested = NormalizedPath::new(entry.path()).join(export_dir);
            if nested.as_path().is_dir() {
                return Ok(nested);
            }
        }
    }
    Err(FbuildError::PackageError(format!(
        "ESP-IDF archive did not produce {} (looked in {})",
        if export_dir.is_empty() {
            "a non-empty root"
        } else {
            export_dir
        },
        root
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux() -> HostPlatform {
        HostPlatform::new(HostOs::Linux, HostArch::X86_64)
    }

    #[test]
    fn s3_profile_installs_exactly_the_xtensa_side_components() {
        let ids: Vec<&str> = IdfProfile::Esp32S3Only
            .components()
            .iter()
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
            assert!(
                pin.url.starts_with("https://github.com/espressif/"),
                "{} must come from an Espressif release: {}",
                pin.component,
                pin.url
            );
            assert!(pin.bytes > 1_000_000, "{} pin has no size", pin.component);
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
        let bytes = IdfProfile::Esp32S3Only.measured_installed_bytes();
        // AC2 of #1970: <= 1.3 GB against `du`, not against an estimate.
        assert!(bytes <= 1_300_000_000, "S3 profile measured {bytes} B");
        assert!(
            bytes >= 1_100_000_000,
            "measurement looks truncated: {bytes} B"
        );
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
        let root = tool_root(tmp.path(), "xtensa-esp-elf").unwrap();
        assert!(root.as_path().ends_with("xtensa-esp-elf"));
        assert!(tool_root(tmp.path(), "riscv32-esp-elf").is_err());
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
}
