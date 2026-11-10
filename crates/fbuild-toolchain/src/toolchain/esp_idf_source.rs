//! Pinned ESP-IDF v5.5.1 **source tree** as one fbuild package (FastLED/fbuild#1970).
//!
//! The tool components ([`super::esp_idf_release`]) are only half of what a
//! delegated `idf.py` build (#1971) needs: the subprocess also has to *be* an
//! ESP-IDF checkout, with `components/`, `tools/idf.py` and the 23 git submodule
//! contents the release pins. This module provisions that tree with no git and
//! no `install.sh`.
//!
//! ## Why the pinned commit's own archive
//!
//! Three candidates were measured against v5.5.1 `fcae3288` on 2026-11-09:
//!
//! | Source | Size | Verdict |
//! |---|---|---|
//! | `esp-idf-v5.5.1.zip` release asset | 1,414 MB | embeds `.git/` (32,664 zip entries) — not a package |
//! | recursive `git clone --depth 1` | 603 MB on disk | makes `git` a provisioning dependency |
//! | commit archive + 23 submodule archives | 61 MB + 47 MB download | **pinned by sha256, no git** |
//!
//! The archive is the pinned commit's own tree, so it is *the* release rather
//! than a summary of it, and every byte is covered by a sha256 we verified here
//! (a re-download on the same day reproduced all 24 digests).
//!
//! ## Submodules
//!
//! GitHub's commit archive ships each submodule path as an empty directory and
//! its `.gitmodules` declares them, so the existing [`SubmodulePlan`] machinery
//! (#1420) is what fills them: one pinned archive per gitlink, verified against
//! its own sha256, extracted with the archive's single top-level directory
//! stripped. The plan is cross-checked against the extracted `.gitmodules`, so
//! a moved or renamed submodule fails the install instead of yielding a tree
//! that compiles right up to a missing third-party header.
//!
//! All 23 gitlinks are pinned, not just the ones an ESP32-S3 link would touch:
//! a source tree that silently lacks `openthread` or a sibling chip's BT library
//! is a trap for the next target, and the unused archives cost 6 MB of the 47.
//!
//! [`SubmodulePlan`]: fbuild_packages_fetch::submodules::SubmodulePlan

use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};

use super::esp_idf_release::{IDF_RELEASE_COMMIT, IDF_RELEASE_VERSION};
use crate::{CacheSubdir, PackageBase, PackageInfo};

/// ESP-IDF v5.5.1 source tree at the pinned commit (GitHub commit archive).
pub const IDF_SOURCE_URL: &str =
    "https://github.com/espressif/esp-idf/archive/fcae32885b0296b32044cb99ecbdc50d98dddb83.tar.gz";

/// sha256 of [`IDF_SOURCE_URL`], measured 2026-11-09 (re-download reproduced it).
pub const IDF_SOURCE_SHA256: &str =
    "ad95862f0796251f51edab9afa9163ac7d14803cf208d5137a2e1fc5aa431e50";

/// Compressed bytes of [`IDF_SOURCE_URL`].
pub const IDF_SOURCE_BYTES: u64 = 61_134_823;

/// Installed bytes of the whole tree (source + every submodule), measured with
/// `du -sb` on 2026-11-09: 449,952,002 B. The submodules cost far more on disk
/// than their 47 MB of archives (sibling-chip BT libraries and Wi-Fi blobs), and
/// `du` counts a hardlinked file once -- relevant here because Espressif ships
/// several tools that way.
pub const IDF_SOURCE_EXTRACTED_BYTES: u64 = 449_952_002;

/// One pinned submodule: the gitlink the release records, and its archive.
#[derive(Debug, Clone, Copy)]
pub struct IdfSubmodulePin {
    /// Path as written in the release's `.gitmodules`.
    pub path: &'static str,
    /// Commit archive of the submodule at exactly that gitlink.
    pub url: &'static str,
    /// sha256 of `url`, measured 2026-11-09.
    pub sha256: &'static str,
    /// Compressed bytes of `url`.
    pub bytes: u64,
}

/// Every gitlink in the pinned commit (`git ls-tree` mode `160000`), verbatim.
pub const IDF_SUBMODULES: &[IdfSubmodulePin] = &[
    IdfSubmodulePin {
        path: "components/bootloader/subproject/components/micro-ecc/micro-ecc",
        url: "https://github.com/kmackay/micro-ecc/archive/24c60e243580c7868f4334a1ba3123481fe1aa48.tar.gz",
        sha256: "e3f5830c8a1c25d5db4d514b703616cc9384ebff7169b3503080ef97213b3d9c",
        bytes: 90_259,
    },
    IdfSubmodulePin {
        path: "components/bt/controller/lib_esp32",
        url: "https://github.com/espressif/esp32-bt-lib/archive/185c7205db1f5082d070d00ea5dfdbb24c291d05.tar.gz",
        sha256: "3c60186c30f471cb341a037463472bda4f660d413884b8b6522c8657a95d159f",
        bytes: 278_915,
    },
    IdfSubmodulePin {
        path: "components/bt/controller/lib_esp32c2/esp32c2-bt-lib",
        url: "https://github.com/espressif/esp32c2-bt-lib/archive/ff958fab2f693e4b5974375cce525945fd238c4f.tar.gz",
        sha256: "682d22e17b329e39e1465efa5d0110b50ec9041587e54e14e23959cf5ab1107b",
        bytes: 1_147_707,
    },
    IdfSubmodulePin {
        path: "components/bt/controller/lib_esp32c3_family",
        url: "https://github.com/espressif/esp32c3-bt-lib/archive/0c68809d62e432427de97b5294f6619307f62f40.tar.gz",
        sha256: "e204c9e3ef9007af2dee15bbf2b23a136320315fb86b9eadce2853a175a9eeca",
        bytes: 1_916_548,
    },
    IdfSubmodulePin {
        path: "components/bt/controller/lib_esp32c5/esp32c5-bt-lib",
        url: "https://github.com/espressif/esp32c5-bt-lib/archive/74cfecd57f600e5e00d175311893d241a6163cdd.tar.gz",
        sha256: "6fd8f3f3a72394a252e904604e9c0a0ef09932692ddcfb18cf32e906303d5e9b",
        bytes: 892_921,
    },
    IdfSubmodulePin {
        path: "components/bt/controller/lib_esp32c6/esp32c6-bt-lib",
        url: "https://github.com/espressif/esp32c6-bt-lib/archive/3655fcdaadb66bb4cd4d497c2dbd4e6e2a4cc696.tar.gz",
        sha256: "b07543116adf24dedbef64b2abe1a2251fe25ba8a23c12fc6de2f3dd5e3b203d",
        bytes: 1_859_983,
    },
    IdfSubmodulePin {
        path: "components/bt/controller/lib_esp32h2/esp32h2-bt-lib",
        url: "https://github.com/espressif/esp32h2-bt-lib/archive/a2ba40fae2462488f65efca042e5cecd9e018c7e.tar.gz",
        sha256: "eb88a1696322cac51a3a7235c4e9c935e1ac739fe51049b91d18f616f67afc7e",
        bytes: 1_927_781,
    },
    IdfSubmodulePin {
        path: "components/bt/esp_ble_mesh/lib/lib",
        url: "https://github.com/espressif/esp-ble-mesh-lib/archive/d9fe7de9a9eb2a4e4c51172b9c303f9d800706fa.tar.gz",
        sha256: "5dbd3c9fd9086d2d56c8b2a4166533304ad379932db560df1ddc0ba9db45d5f0",
        bytes: 4_056_772,
    },
    IdfSubmodulePin {
        path: "components/bt/host/nimble/nimble",
        url: "https://github.com/espressif/esp-nimble/archive/b45dcedcafb7888174c3567002c36b342ec0b723.tar.gz",
        sha256: "4fc7767c4d93023488fa8e06dd52d6139df5f855d240417ae07a60023a4dd7e2",
        bytes: 1_845_863,
    },
    IdfSubmodulePin {
        path: "components/cmock/CMock",
        url: "https://github.com/ThrowTheSwitch/CMock/archive/eeecc49ce8af123cf8ad40efdb9673e37b56230f.tar.gz",
        sha256: "b39e8cb357d44630e80800990d731108ffc517ca01632a08d7cc9e3fcd5408f3",
        bytes: 434_529,
    },
    IdfSubmodulePin {
        path: "components/esp_coex/lib",
        url: "https://github.com/espressif/esp-coex-lib/archive/2d68674e3d522fb025e4666217f9cc1ca2af9399.tar.gz",
        sha256: "1a9f5e3a619527468603a1d5d98cddaffbeaeadf04ae42787485b7ed12d86098",
        bytes: 215_720,
    },
    IdfSubmodulePin {
        path: "components/esp_phy/lib",
        url: "https://github.com/espressif/esp-phy-lib/archive/45ce2703403784b4c3983a52e453988d61eb0e9d.tar.gz",
        sha256: "fb1d68d49e5431e9e0c81abf7ed15a31f4401f964eaa9574d9c96da469d5eddf",
        bytes: 2_948_146,
    },
    IdfSubmodulePin {
        path: "components/esp_wifi/lib",
        url: "https://github.com/espressif/esp32-wifi-lib/archive/c1f74dcf9151ae21b7a3850b90952796a2806358.tar.gz",
        sha256: "0867b4d3cbb8d9bc800cd36aacaa2663ed01bae9e13c4316aed5e402981c695c",
        bytes: 9_820_830,
    },
    IdfSubmodulePin {
        path: "components/heap/tlsf",
        url: "https://github.com/espressif/tlsf/archive/2867f6883a12920b1969ff9624c0ab0e4185c2ce.tar.gz",
        sha256: "a9dcb699a671b496bfb6a457bf750de25186a6a412f0602c6182ba67f248b536",
        bytes: 15_232,
    },
    IdfSubmodulePin {
        path: "components/json/cJSON",
        url: "https://github.com/DaveGamble/cJSON/archive/8f2beb57ddad1f94bed899790b00f46df893ccac.tar.gz",
        sha256: "bc69a9e0796bb6e3434aeadbb49d3e37c4b7aec34fdb306e3f55b275ca7c6acb",
        bytes: 356_275,
    },
    IdfSubmodulePin {
        path: "components/lwip/lwip",
        url: "https://github.com/espressif/esp-lwip/archive/ebabd6d79c120c5d4415755aab1d908d8b5fa372.tar.gz",
        sha256: "609a4db4e65c7e67acad8325bc48ecd6913cf10c606ff8c3e8fc25589d9d2a1c",
        bytes: 1_634_786,
    },
    IdfSubmodulePin {
        path: "components/mbedtls/mbedtls",
        url: "https://github.com/espressif/mbedtls/archive/b5d87eaa6748b7a6fa70593178c08b4480e9b71e.tar.gz",
        sha256: "e6beb4ed862c83daba07f0d8091c0c361c843623b06674b2d8effe00e8e7aa9d",
        bytes: 6_400_544,
    },
    IdfSubmodulePin {
        path: "components/mqtt/esp-mqtt",
        url: "https://github.com/espressif/esp-mqtt/archive/6af4446a48ea7fa54948edc4e62277ed70abb6e1.tar.gz",
        sha256: "29166d1e9c00267e73f3bb525ce921ad4ab80eb349ba1203e3a4d1a402b5e7eb",
        bytes: 68_387,
    },
    IdfSubmodulePin {
        path: "components/openthread/lib",
        url: "https://github.com/espressif/esp-thread-lib/archive/ec0296c44f628b8e1d1cb78898fe62baa2f90d13.tar.gz",
        sha256: "006963483fc9a7fbb09d4a53ba16619adf7eeccb795802c962fd1af246b101da",
        bytes: 860_877,
    },
    IdfSubmodulePin {
        path: "components/openthread/openthread",
        url: "https://github.com/espressif/openthread/archive/b945928d722177cd9caeab2e1025499628c101ef.tar.gz",
        sha256: "a5b979a4ebb2756926bb0c0a1d0085751dedaa476d97169f49471b488a6c4dbf",
        bytes: 9_656_601,
    },
    IdfSubmodulePin {
        path: "components/protobuf-c/protobuf-c",
        url: "https://github.com/protobuf-c/protobuf-c/archive/abc67a11c6db271bedbb9f58be85d6f4e2ea8389.tar.gz",
        sha256: "6f41b467d7b7242bb201f6466ba433f6eab943e2b6fc53bb753340632c9e45e2",
        bytes: 134_746,
    },
    IdfSubmodulePin {
        path: "components/spiffs/spiffs",
        url: "https://github.com/pellepl/spiffs/archive/0dbb3f71c5f6fae3747a9d935372773762baf852.tar.gz",
        sha256: "ad2f6bd71024d8aa917b860fc0e1e242c10984219ed3460be09a375803a72517",
        bytes: 100_473,
    },
    IdfSubmodulePin {
        path: "components/unity/unity",
        url: "https://github.com/ThrowTheSwitch/Unity/archive/bf560290f6020737eafaa8b5cbd2177c3956c03f.tar.gz",
        sha256: "441fae1f1e49f545a871508bf096f50c69e602a9df54ef8a8a4972f52389b77e",
        bytes: 305_629,
    },
];

/// Files that only a complete tree has: the release marker, `idf.py`, the build
/// system's own entry component, and one submodule the S3 Wi-Fi link needs.
const SOURCE_MARKERS: &[&str] = &[
    // The release's own version declaration: IDF is readable without `.git`
    // (`version.txt` is optional, `idf_tools.py` falls back to the header),
    // so this file is what proves *which* release the tree is.
    "tools/cmake/version.cmake",
    "tools/idf.py",
    "components/esp_system/CMakeLists.txt",
    "components/esp_common/include/esp_idf_version.h",
    // One submodule the ESP32-S3 Wi-Fi link cannot work without.
    "components/mbedtls/mbedtls/CMakeLists.txt",
];

/// The pinned ESP-IDF source tree, installed as one fbuild package.
pub struct EspIdfSource {
    base: PackageBase,
}

impl EspIdfSource {
    /// Resolve the pinned source against the default cache.
    pub fn new(project_dir: &Path) -> Result<Self> {
        Self::build(project_dir, None)
    }

    /// Install into an explicit cache root (tests and the end-to-end proof).
    #[cfg(test)]
    pub(super) fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Result<Self> {
        Self::build(project_dir, Some(cache_root))
    }

    fn build(project_dir: &Path, cache_root: Option<&Path>) -> Result<Self> {
        // `<release>/source`: the release is part of the cache key, so two IDF
        // versions install side by side and neither shares an entry (#1843).
        let cache_key = format!("esp-idf-{IDF_RELEASE_VERSION}/source");
        let name = format!("esp-idf-{IDF_RELEASE_VERSION}-source");
        let version = format!("{IDF_RELEASE_VERSION}+{IDF_RELEASE_COMMIT}");
        let mut base = match cache_root {
            Some(root) => PackageBase::with_cache_root(
                &name,
                &version,
                IDF_SOURCE_URL,
                &cache_key,
                Some(IDF_SOURCE_SHA256),
                CacheSubdir::Toolchains,
                project_dir,
                root,
            ),
            None => PackageBase::new(
                &name,
                &version,
                IDF_SOURCE_URL,
                &cache_key,
                Some(IDF_SOURCE_SHA256),
                CacheSubdir::Toolchains,
                project_dir,
            ),
        };
        for pin in IDF_SUBMODULES {
            base = base.with_submodule_source(pin.path, pin.url, pin.sha256);
        }
        Ok(Self { base })
    }

    /// The pinned submodule table this package installs.
    pub fn submodules(&self) -> &'static [IdfSubmodulePin] {
        IDF_SUBMODULES
    }

    /// Where the tree is (or would be) installed.
    pub fn install_path(&self) -> NormalizedPath {
        NormalizedPath::new(self.base.install_path())
    }

    /// True only when the archive is extracted **and** the tree is complete:
    /// the sentinel alone would accept a root whose submodules never landed.
    pub fn is_installed(&self) -> bool {
        self.base.is_cached() && source_root(&self.base.install_path()).is_ok()
    }

    /// Install the pinned source and return the tree root.
    ///
    /// Inherent, not a `Package` impl: `Package::ensure_installed` is typed
    /// `std::path::PathBuf`, which new files may not use (`ban_std_pathbuf`).
    pub async fn ensure_normalized(&self) -> Result<NormalizedPath> {
        if self.is_installed() {
            return source_root(&self.base.install_path());
        }
        let installed = self
            .base
            .staged_install(|root: &Path| source_root(root).map(|_| ()))
            .await?;
        source_root(&installed)
    }

    /// Package metadata for install reports and logging.
    pub fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

/// The tree root inside the install directory.
///
/// GitHub's archive wraps everything in `<repo>-<sha>/`, and `staged_install`
/// renames the staging directory rather than flattening it, so the root is
/// either the install directory itself or its single child.
///
/// Returns the error, rather than a path that may be incomplete, so a caller
/// cannot "successfully" build against a tree that is missing its submodules.
fn source_root(root: &Path) -> Result<NormalizedPath> {
    let mut candidates = vec![root.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(root) {
        candidates.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir()),
        );
    }
    for candidate in &candidates {
        let missing: Vec<&str> = SOURCE_MARKERS
            .iter()
            .copied()
            .filter(|marker| !candidate.join(marker).is_file())
            .collect();
        if missing.is_empty() {
            return Ok(NormalizedPath::new(candidate.clone()));
        }
    }
    Err(FbuildError::PackageError(format!(
        "ESP-IDF source at {} is incomplete: neither it nor its child directories \
         contain {} (looked in {:?})",
        root.display(),
        SOURCE_MARKERS.join(", "),
        candidates
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both pinned tables must describe the same release: a source tree from one
    /// commit beside tool components from another is not an installable IDF.
    #[test]
    fn the_source_pin_is_the_release_the_tools_are_pinned_to() {
        assert!(
            IDF_SOURCE_URL.contains(IDF_RELEASE_COMMIT),
            "source URL must name the pinned commit: {IDF_SOURCE_URL}"
        );
        assert_eq!(IDF_SOURCE_SHA256.len(), 64);
        // A truncated pin in this table is a copy-paste mistake, and the constant
        // is the only guard before a 108 MB download; a `const` block keeps the
        // check compile-time instead of an assertion clippy can fold away.
        const { assert!(IDF_SOURCE_BYTES > 40_000_000) };
    }

    #[test]
    fn every_gitlink_is_pinned_to_its_own_commit_archive() {
        for pin in IDF_SUBMODULES {
            assert!(
                !pin.path.starts_with('/') && !pin.path.contains(".."),
                "{}",
                pin.path
            );
            let Some(rest) = pin.url.strip_prefix("https://github.com/") else {
                panic!("{} is not a GitHub commit archive: {}", pin.path, pin.url);
            };
            let fields: Vec<&str> = rest.split('/').collect();
            assert_eq!(fields.len(), 4, "{} url shape: {}", pin.path, pin.url);
            assert_eq!(fields[2], "archive");
            let Some(sha) = fields[3].strip_suffix(".tar.gz") else {
                panic!("{} archive is not a tarball: {}", pin.path, pin.url);
            };
            assert_eq!(sha.len(), 40, "{} gitlink: {sha}", pin.path);
            assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
            assert_eq!(pin.sha256.len(), 64, "{} sha256", pin.path);
            assert!(pin.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(pin.bytes > 0, "{} has no measured size", pin.path);
        }
    }

    #[test]
    fn all_declared_gitlinks_are_pinned_exactly_once() {
        let mut paths: Vec<&str> = IDF_SUBMODULES.iter().map(|p| p.path).collect();
        let total = paths.len();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(total, paths.len(), "duplicate submodule path");
        assert_eq!(total, 23, "v5.5.1 has 23 gitlinks");
        // The three an ESP32-S3 Wi-Fi build cannot link without.
        for required in [
            "components/mbedtls/mbedtls",
            "components/lwip/lwip",
            "components/esp_phy/lib",
        ] {
            assert!(paths.contains(&required), "{required} must be pinned");
        }
    }

    #[test]
    fn the_source_package_costs_far_less_than_a_git_clone() {
        let total: u64 = IDF_SOURCE_BYTES + IDF_SUBMODULES.iter().map(|p| p.bytes).sum::<u64>();
        // Measured: 61.1 MB + 47.0 MB = 108 MB versus 603 MB for `--depth 1`
        // plus 23 submodule checkouts. Keep the package a package.
        assert!(total < 150_000_000, "source pins total {total} B");
        assert!(
            IDF_SOURCE_EXTRACTED_BYTES > total,
            "extracted tree < archives"
        );
        // The measured tree is what #1970 quotes for the source component: a
        // truncated measurement would silently understate the install.
        assert!(
            (400_000_000..600_000_000).contains(&IDF_SOURCE_EXTRACTED_BYTES),
            "source tree measured {IDF_SOURCE_EXTRACTED_BYTES} B"
        );
    }

    #[test]
    fn two_idf_releases_install_side_by_side() {
        let cache = tempfile::TempDir::new().unwrap();
        let source = EspIdfSource::with_cache_root(cache.path(), cache.path()).unwrap();
        assert_eq!(
            source
                .install_path()
                .as_path()
                .file_name()
                .and_then(|n| n.to_str()),
            Some(format!("{IDF_RELEASE_VERSION}+{IDF_RELEASE_COMMIT}").as_str()),
        );
        assert!(
            source.install_path().as_path().is_absolute(),
            "install path must be rooted: {}",
            source.install_path()
        );
    }

    #[test]
    fn a_tree_without_its_submodules_is_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join(format!("esp-idf-{IDF_RELEASE_COMMIT}"));
        for marker in SOURCE_MARKERS.iter().filter(|m| !m.contains("mbedtls")) {
            let path = nested.join(marker);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
        }
        // Submodule directory present but empty: exactly what the raw archive
        // extracts to, and exactly what the plan fills.
        std::fs::create_dir_all(nested.join("components/mbedtls/mbedtls")).unwrap();
        assert!(source_root(tmp.path()).is_err());
        std::fs::write(
            nested.join("components/mbedtls/mbedtls/CMakeLists.txt"),
            b"x",
        )
        .unwrap();
        let root = source_root(tmp.path()).unwrap();
        assert!(
            root.as_path()
                .ends_with(format!("esp-idf-{IDF_RELEASE_COMMIT}"))
        );
    }

    #[test]
    fn an_empty_install_directory_is_not_a_tree() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(source_root(tmp.path()).is_err());
    }

    /// Real download + sha256 verify + extract of the source and all 23
    /// submodule archives (~108 MB). Run with
    /// `soldr cargo test -p fbuild-toolchain --lib -- --ignored esp_idf_source_tree`.
    #[tokio::test]
    #[ignore = "downloads the pinned ESP-IDF source and 23 submodule archives"]
    async fn esp_idf_source_tree_installs_with_every_submodule() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = EspIdfSource::with_cache_root(dir.path(), dir.path()).unwrap();
        assert!(!source.is_installed());
        let root = source.ensure_normalized().await.unwrap();
        assert!(source.is_installed());
        for marker in SOURCE_MARKERS {
            assert!(root.join(marker).as_path().is_file(), "missing {marker}");
        }
        // The tree is the *pinned* release, not merely a plausible ESP-IDF.
        let version =
            std::fs::read_to_string(root.join("tools/cmake/version.cmake").as_path()).unwrap();
        let expected = IDF_RELEASE_VERSION
            .trim_start_matches('v')
            .replace('.', " ");
        let mut parts = expected.split(' ');
        for (name, value) in [
            ("IDF_VERSION_MAJOR", parts.next().unwrap()),
            ("IDF_VERSION_MINOR", parts.next().unwrap()),
            ("IDF_VERSION_PATCH", parts.next().unwrap()),
        ] {
            assert!(
                version.contains(&format!("set({name} {value})")),
                "{name} {value} missing from version.cmake"
            );
        }
        // The plan filled every declared submodule: nothing is left empty.
        let declared = fbuild_packages_fetch::submodules::declared_submodule_paths(
            &std::fs::read_to_string(root.join(".gitmodules").as_path()).unwrap(),
        );
        assert_eq!(declared.len(), IDF_SUBMODULES.len(), "gitlink set moved");
        for path in &declared {
            let entries = std::fs::read_dir(root.join(path).as_path())
                .unwrap_or_else(|e| panic!("{path} not extracted: {e}"))
                .count();
            assert!(entries > 0, "{path} is empty");
        }
        // Second call is a cache hit: same tree, no re-download.
        let again = source.ensure_normalized().await.unwrap();
        assert_eq!(root, again);
    }
}
