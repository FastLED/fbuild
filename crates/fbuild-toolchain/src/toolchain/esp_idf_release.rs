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
    /// CMake, the build system generator `idf.py` drives.
    Cmake,
    /// Ninja, the build tool CMake generates for. Both are `install:
    /// "on_request"` in `tools.json`: ESP-IDF expects the *host* to provide
    /// them on Linux and macOS, and installs them itself only on Windows.
    /// Pinning both is what makes an fbuild-provisioned release self-contained
    /// -- without them `idf.py build` dead-ends on `cmake not found`, which is
    /// exactly the host dependency `install.sh` left behind.
    Ninja,
}

/// Where a component's tool root sits inside its extracted archive.
///
/// `idf_tools.py` deletes each archive's container directory before exporting
/// `export_paths`, so upstream never has to name it. fbuild keeps the archive
/// exactly as published and therefore has to *find* the root; this is that
/// rule, and it is per-archive data rather than a heuristic applied to all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootRule {
    /// The archive root itself, holding data rather than a program.
    ArchiveRoot,
    /// A fixed directory name, at the archive root or one level down
    /// (`<name>-<version>/<name>/`).
    Named(&'static str),
    /// The archive's only payload is a binary at its root, so the tool root is
    /// the root itself and belongs on `PATH` unchanged
    /// (`ninja-linux.zip` unpacks as one `ninja` file).
    Flat,
    /// The directory that owns this archive-relative executable. CMake's
    /// container names its own release (`cmake-3.30.2-linux-x86_64`) and on
    /// macOS buries the binary under `CMake.app/Contents/bin`, so neither the
    /// directory nor its depth can be spelled as a constant.
    Owning(&'static str),
}

impl IdfComponent {
    /// Every target toolchain component [`IdfProfile::Esp32S3Only`] installs, in
    /// install order. This is the set #1970's 1.3 GB budget was measured on.
    pub const ALL: [Self; 4] = [
        Self::XtensaEspElf,
        Self::Esp32UlpElf,
        Self::EspRomElfs,
        Self::OpenocdEsp32,
    ];

    /// The host build tools, installed after the target toolchains.
    pub const BUILD_TOOLS: [Self; 2] = [Self::Cmake, Self::Ninja];

    /// `tools.json` name — also the directory name most archives export.
    pub const fn id(self) -> &'static str {
        match self {
            Self::XtensaEspElf => "xtensa-esp-elf",
            Self::Esp32UlpElf => "esp32ulp-elf",
            Self::EspRomElfs => "esp-rom-elfs",
            Self::OpenocdEsp32 => "openocd-esp32",
            Self::Cmake => "cmake",
            Self::Ninja => "ninja",
        }
    }

    /// True for a host build tool rather than a target cross toolchain.
    ///
    /// The split exists because they answer different budgets: the cross
    /// toolchains are what #1970 AC2 capped at 1.3 GB, while CMake and Ninja
    /// are ~154 MB of host programs that any `idf.py build` needs.
    pub const fn is_build_tool(self) -> bool {
        matches!(self, Self::Cmake | Self::Ninja)
    }

    const fn root_rule(self) -> RootRule {
        match self {
            Self::EspRomElfs => RootRule::ArchiveRoot,
            Self::XtensaEspElf | Self::Esp32UlpElf | Self::OpenocdEsp32 => {
                RootRule::Named(self.id())
            }
            Self::Cmake => RootRule::Owning("bin/cmake"),
            Self::Ninja => RootRule::Flat,
        }
    }

    /// The directory this component contributes to `PATH` under `root`, if
    /// any: `root/bin` for an archive that ships a `bin/`, `root` itself for a
    /// [flat](RootRule::Flat) one, and nothing for data-only components.
    ///
    /// Derived from the installed tree rather than from a table, so a component
    /// added to the profile cannot silently never reach `PATH`; the FLAT case
    /// is the one the archive itself decides.
    pub fn path_dir(self, root: &NormalizedPath) -> Option<NormalizedPath> {
        match self.root_rule() {
            RootRule::ArchiveRoot => None,
            RootRule::Flat => Some(root.clone()),
            RootRule::Named(_) | RootRule::Owning(_) => {
                let bin = root.join("bin");
                bin.as_path().is_dir().then_some(bin)
            }
        }
    }

    /// The program this component must produce, relative to its tool root.
    /// `None` for a data-only component: `esp-rom-elfs` ships ROM symbol files
    /// the linker reads by path, not something a `PATH` entry can run.
    ///
    /// Used as the install marker, so "the archive extracted" is not mistaken
    /// for "the tool is there" -- a half-extracted CMake tree has a `bin/` that
    /// is missing its `cmake`.
    fn executable(self) -> Option<String> {
        let rel = match self {
            Self::Cmake => "bin/cmake",
            Self::Ninja => "ninja",
            _ => return None,
        };
        Some(relative_native_name(rel))
    }
}

/// `rel` with the host's executable suffix on its leaf (`bin/cmake` ->
/// `bin/cmake.exe` on Windows), so the marker matches what the extraction
/// actually wrote.
fn relative_native_name(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((dir, stem)) => format!(
            "{}/{}",
            dir,
            fbuild_core::platform::executable::native_name(stem)
        ),
        None => fbuild_core::platform::executable::native_name(rel),
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

/// Installed bytes of the ESP32-S3 profile's **target toolchains**, measured with
/// `du -sb` on linux-amd64 (2026-11-09, ESP-IDF v5.5.1 as installed by
/// `install.sh` into a scratch tree). This is the set #1970 AC2 capped at
/// 1.3 GB.
pub const S3_MEASURED_INSTALLED_BYTES: &[(&str, u64)] = &[
    ("xtensa-esp-elf", 1_103_937_348),
    ("esp32ulp-elf", 31_188_493),
    ("esp-rom-elfs", 6_918_628),
    ("openocd-esp32", 7_153_401),
];

/// Installed bytes of the two host build tools, measured with `du -sb` on
/// linux-amd64 (2026-11-10, extracted from the pinned archives).
pub const BUILD_TOOLS_MEASURED_INSTALLED_BYTES: &[(&str, u64)] =
    &[("cmake", 153_276_700), ("ninja", 273_768)];

/// Every component [`IdfProfile::Esp32S3Only`] installs, in install order:
/// target toolchains first, then the host build tools they are driven by.
const S3_COMPONENTS: [IdfComponent; 6] = [
    IdfComponent::XtensaEspElf,
    IdfComponent::Esp32UlpElf,
    IdfComponent::EspRomElfs,
    IdfComponent::OpenocdEsp32,
    IdfComponent::Cmake,
    IdfComponent::Ninja,
];

/// Which components an install pulls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdfProfile {
    /// Xtensa + ULP + ROM ELFs + OpenOCD: everything an ESP32-S3 build needs and
    /// nothing for the other eight targets.
    Esp32S3Only,
}

impl IdfProfile {
    /// Components this profile installs: the target toolchains, then the host
    /// build tools.
    ///
    /// `idf.py build` needs both halves, so the profile owns both; the two
    /// budgets stay separable through [`IdfComponent::is_build_tool`].
    pub fn components(self) -> &'static [IdfComponent] {
        match self {
            Self::Esp32S3Only => &S3_COMPONENTS,
        }
    }

    /// Measured installed bytes of the target toolchains
    /// (`S3_MEASURED_INSTALLED_BYTES`), the set #1970 AC2 measured.
    pub fn measured_installed_bytes(self) -> u64 {
        match self {
            Self::Esp32S3Only => S3_MEASURED_INSTALLED_BYTES.iter().map(|(_, b)| b).sum(),
        }
    }

    /// Measured installed bytes of the host build tools.
    pub fn measured_build_tools_bytes(self) -> u64 {
        match self {
            Self::Esp32S3Only => BUILD_TOOLS_MEASURED_INSTALLED_BYTES
                .iter()
                .map(|(_, b)| b)
                .sum(),
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
        "no pinned ESP-IDF {} archive for {:?} {:?} (pinned: linux-amd64, linux-arm64, macos, macos-arm64, win32; linux-armel/armhf/i686 are deliberately unpinned)",
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
    // CMake 3.30.2, `tools.json` `status: recommended`. Kitware's own release
    // assets; the 3.16.3 entry `tools.json` also carries is `supported`, not
    // recommended, and is not pinned.
    EspIdfPin {
        component: "cmake",
        platform: "linux-amd64",
        version: "3.30.2",
        sha256: "cdd7fb352605cee3ae53b0e18b5929b642900e33d6b0173e19f6d4f2067ebf16",
        bytes: 53635506,
        url: "https://github.com/Kitware/CMake/releases/download/v3.30.2/cmake-3.30.2-linux-x86_64.tar.gz",
    },
    EspIdfPin {
        component: "cmake",
        platform: "linux-arm64",
        version: "3.30.2",
        sha256: "d18f50f01b001303d21f53c6c16ff12ee3aa45df5da1899c2fe95be7426aa026",
        bytes: 54889935,
        url: "https://github.com/Kitware/CMake/releases/download/v3.30.2/cmake-3.30.2-linux-aarch64.tar.gz",
    },
    EspIdfPin {
        component: "cmake",
        platform: "macos",
        version: "3.30.2",
        sha256: "c6fdda745f9ce69bca048e91955c7d043ba905d6388a62e0ff52b681ac17183c",
        bytes: 79199037,
        url: "https://github.com/Kitware/CMake/releases/download/v3.30.2/cmake-3.30.2-macos-universal.tar.gz",
    },
    EspIdfPin {
        component: "cmake",
        platform: "macos-arm64",
        version: "3.30.2",
        sha256: "c6fdda745f9ce69bca048e91955c7d043ba905d6388a62e0ff52b681ac17183c",
        bytes: 79199037,
        url: "https://github.com/Kitware/CMake/releases/download/v3.30.2/cmake-3.30.2-macos-universal.tar.gz",
    },
    EspIdfPin {
        // `tools.json` publishes the same x86_64 archive under `win32` and
        // `win64`; fbuild's Windows host key is `win32`.
        component: "cmake",
        platform: "win32",
        version: "3.30.2",
        sha256: "48bf4b3dc2d668c578e0884cac7878e146b036ca6b5ce4f8b5572f861b004c25",
        bytes: 45404613,
        url: "https://github.com/Kitware/CMake/releases/download/v3.30.2/cmake-3.30.2-windows-x86_64.zip",
    },
    // Ninja 1.12.1, one single-file zip per host.
    EspIdfPin {
        component: "ninja",
        platform: "linux-amd64",
        version: "1.12.1",
        sha256: "6f98805688d19672bd699fbbfa2c2cf0fc054ac3df1f0e6a47664d963d530255",
        bytes: 123084,
        url: "https://github.com/ninja-build/ninja/releases/download/v1.12.1/ninja-linux.zip",
    },
    EspIdfPin {
        component: "ninja",
        platform: "macos",
        version: "1.12.1",
        sha256: "89a287444b5b3e98f88a945afa50ce937b8ffd1dcc59c555ad9b1baf855298c9",
        bytes: 281130,
        url: "https://github.com/ninja-build/ninja/releases/download/v1.12.1/ninja-mac.zip",
    },
    EspIdfPin {
        component: "ninja",
        platform: "macos-arm64",
        version: "1.12.1",
        sha256: "89a287444b5b3e98f88a945afa50ce937b8ffd1dcc59c555ad9b1baf855298c9",
        bytes: 281130,
        url: "https://github.com/ninja-build/ninja/releases/download/v1.12.1/ninja-mac.zip",
    },
    EspIdfPin {
        // `tools.json` spells this one `win64` (last entry of a `versions`
        // object); fbuild's key for the same host is `win32`.
        component: "ninja",
        platform: "win32",
        version: "1.12.1",
        sha256: "f550fec705b6d6ff58f2db3c374c2277a37691678d6aba463adcbb129108467a",
        bytes: 275425,
        url: "https://github.com/ninja-build/ninja/releases/download/v1.12.1/ninja-win.zip",
    },
];

/// An installable ESP-IDF tool component.
pub struct EspIdfTool {
    base: PackageBase,
    component: IdfComponent,
}

impl EspIdfTool {
    /// Resolve the pinned archive for `component` on the current host.
    pub fn new(project_dir: &Path, component: IdfComponent) -> Result<Self> {
        Self::build(project_dir, component, host::current(), None)
    }

    /// Install into an explicit cache root (tests and the end-to-end proof).
    #[cfg(test)]
    pub(super) fn with_cache_root(
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
        Ok(Self { base, component })
    }

    /// The pinned component.
    pub fn component(&self) -> IdfComponent {
        self.component
    }

    /// Where the component is (or would be) installed.
    pub fn install_path(&self) -> NormalizedPath {
        NormalizedPath::new(self.base.install_path())
    }

    /// True only when the archive is extracted **and** the component's root
    /// exists inside it: a half-extracted tree is not an install, and a cache
    /// hit that lost the root must be repaired, not trusted.
    pub fn is_installed(&self) -> bool {
        self.base.is_cached() && tool_root(&self.base.install_path(), self.component).is_ok()
    }

    /// Install the pinned archive and return the component's `export` root.
    ///
    /// Inherent, not a `Package` impl: `Package::ensure_installed` is typed
    /// `std::path::PathBuf`, and `dylints/ban_std_pathbuf` holds that legacy
    /// signature on a shrink-only allowlist of pre-existing files. New code
    /// returns [`NormalizedPath`]; a consumer that needs the trait can convert
    /// at its own legacy boundary (see `MarkerArchivePackage`).
    pub async fn ensure_normalized(&self) -> Result<NormalizedPath> {
        let component = self.component;
        if self.is_installed() {
            return tool_root(&self.base.install_path(), component);
        }
        let installed = self
            .base
            .staged_install(move |root: &Path| tool_root(root, component).map(|_| ()))
            .await?;
        tool_root(&installed, component)
    }

    /// Package metadata for install reports and logging.
    pub fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

/// The tool root inside an extracted archive, per the component's
/// [`RootRule`], or the error naming what the archive failed to produce.
///
/// Returns the error, rather than a path that may not exist, so the caller
/// cannot install "successfully" into a directory the release never produced.
fn tool_root(root: &Path, component: IdfComponent) -> Result<NormalizedPath> {
    let root = NormalizedPath::new(root);
    match component.root_rule() {
        RootRule::ArchiveRoot => Ok(root),
        RootRule::Named(name) => match named_child(&root, name) {
            Some(found) => Ok(found),
            None => Err(missing_component(component, &root, name)),
        },
        RootRule::Flat => match component.executable() {
            Some(exe) if root.join(&exe).as_path().is_file() => Ok(root),
            Some(exe) => Err(missing_component(component, &root, &exe)),
            None => Ok(root),
        },
        RootRule::Owning(rel) => {
            let probe = relative_native_name(rel);
            match owning_dir(root.as_path(), &probe, OWNING_SEARCH_DEPTH) {
                Some(found) => Ok(found),
                None => Err(missing_component(component, &root, &probe)),
            }
        }
    }
}

/// Depth budget for [`RootRule::Owning`]: enough for macOS CMake, whose binary
/// is `<container>/CMake.app/Contents/bin/cmake` (three levels below the
/// archive root), and small enough that the search cannot walk a toolchain.
const OWNING_SEARCH_DEPTH: u8 = 4;

/// The directory at the archive root or one level down called `name`
/// (Espressif tarballs nest as `<name>-<version>/<name>/`).
fn named_child(root: &NormalizedPath, name: &str) -> Option<NormalizedPath> {
    let direct = root.join(name);
    if direct.as_path().is_dir() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(root.as_path()).ok()?;
    entries
        .flatten()
        .map(|entry| NormalizedPath::new(entry.path()).join(name))
        .find(|nested| nested.as_path().is_dir())
}

/// The shallowest directory under `root` that holds `rel`, searched
/// depth-first with a bounded depth.
fn owning_dir(root: &Path, rel: &str, depth: u8) -> Option<NormalizedPath> {
    let here = NormalizedPath::new(root);
    if here.join(rel).as_path().is_file() {
        return Some(here);
    }
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(root).ok()?;
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .find_map(|dir| owning_dir(&dir, rel, depth - 1))
}

fn missing_component(component: IdfComponent, root: &NormalizedPath, wanted: &str) -> FbuildError {
    FbuildError::PackageError(format!(
        "ESP-IDF archive did not produce {} (expected `{}`, looked in {})",
        component.id(),
        wanted,
        root
    ))
}

#[cfg(test)]
#[path = "esp_idf_release_tests.rs"]
mod esp_idf_release_tests;
