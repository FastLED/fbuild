//! Pinned ESP-IDF v5.5.6 **source tree** as one fbuild package (FastLED/fbuild#1970).
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

/// ESP-IDF v5.5.6 source tree at the pinned commit (GitHub commit archive).
pub const IDF_SOURCE_URL: &str =
    "https://github.com/espressif/esp-idf/archive/880a203ae603d08a94d06aa01e0d6761958e5e9e.tar.gz";

/// sha256 of [`IDF_SOURCE_URL`], measured 2026-11-22 (re-download reproduced it).
pub const IDF_SOURCE_SHA256: &str =
    "009b1c0623f7ee608ad20d26701cfb2be0f1cd5e2861fb8c1421de69c4a120d8";

/// Compressed bytes of [`IDF_SOURCE_URL`].
pub const IDF_SOURCE_BYTES: u64 = 65_204_565;

/// Installed bytes of the whole tree (source + every submodule), measured with
/// `du -sb` on 2026-11-22: 428,094,401 B. The submodules cost far more on disk
/// than their 47 MB of archives (sibling-chip BT libraries and Wi-Fi blobs), and
/// `du` counts a hardlinked file once -- relevant here because Espressif ships
/// several tools that way.
pub const IDF_SOURCE_EXTRACTED_BYTES: u64 = 428_094_401;

/// One pinned submodule: the gitlink the release records, and its archive.
#[derive(Debug, Clone, Copy)]
pub struct IdfSubmodulePin {
    /// Path as written in the release's `.gitmodules`.
    pub path: &'static str,
    /// Commit archive of the submodule at exactly that gitlink.
    pub url: &'static str,
    /// sha256 of `url`, measured 2026-11-22.
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
        url: "https://github.com/espressif/esp32-bt-lib/archive/e4281c149690558467f75137d64bd5334076e7e1.tar.gz",
        sha256: "5a45266392e418009e5feed9853241e308a6f7292d6168f6e1c5e08a712852ab",
        bytes: 297_338,
    },
    IdfSubmodulePin {
        path: "components/bt/controller/lib_esp32c2/esp32c2-bt-lib",
        url: "https://github.com/espressif/esp32c2-bt-lib/archive/6a5c0b8bfca1a2e984e89431b8ebf9b521dad5ea.tar.gz",
        sha256: "6e5d7b8e10d6dae8705859087980376d29330326193e976b1a43c0302d062c4e",
        bytes: 703_211,
    },
    IdfSubmodulePin {
        path: "components/bt/controller/lib_esp32c3_family",
        url: "https://github.com/espressif/esp32c3-bt-lib/archive/0a08c4b32f3666003080b662a1a61794da24ff0f.tar.gz",
        sha256: "4d54b26bd38c9a9e145d7c937d87ff53adbdbd11b6570f4b643bcc0fa377fb7f",
        bytes: 1_973_605,
    },
    IdfSubmodulePin {
        path: "components/bt/controller/lib_esp32c5/esp32c5-bt-lib",
        url: "https://github.com/espressif/esp32c5-bt-lib/archive/0b5cb2d7cfb4078e951da36a695bc4fb37e52391.tar.gz",
        sha256: "a2f9dcd44545183ccc9c8ba64fbda7f2aa1290a4ada1cb6f4699955743657deb",
        bytes: 628_861,
    },
    IdfSubmodulePin {
        path: "components/bt/controller/lib_esp32c6/esp32c6-bt-lib",
        url: "https://github.com/espressif/esp32c6-bt-lib/archive/a00f2d0a19c54b9ca31358754fbb8b9a08cbb92c.tar.gz",
        sha256: "5c9054656a58253878f7b4812ed0c0d0ce6b7b9efeacd3dd0b9ce9b9ae64b808",
        bytes: 1_238_118,
    },
    IdfSubmodulePin {
        path: "components/bt/controller/lib_esp32h2/esp32h2-bt-lib",
        url: "https://github.com/espressif/esp32h2-bt-lib/archive/71685bf7530058c468ec9e28b2eebbe71df88741.tar.gz",
        sha256: "f88cccab7c79d2c9a82faaa8467b91b3c467c663429c16aa47da34ef47c7531d",
        bytes: 2_280_122,
    },
    IdfSubmodulePin {
        path: "components/bt/esp_ble_mesh/lib/lib",
        url: "https://github.com/espressif/esp-ble-mesh-lib/archive/428709f9e9b70f1cbc52931e433cf68c7673b0df.tar.gz",
        sha256: "538fbbea76f483f0ff57ad4ea7c85f0c98d0c12ccefa962f639154e01e7641bd",
        bytes: 4_214_470,
    },
    IdfSubmodulePin {
        path: "components/bt/host/nimble/nimble",
        url: "https://github.com/espressif/esp-nimble/archive/71a1fd7bfd0654c6d174cc3f9cb007864f767cbc.tar.gz",
        sha256: "403708fbbc787da2346810b5039faf53e4ba8fad7d93ebad1032f0f8870b9b35",
        bytes: 1_966_078,
    },
    IdfSubmodulePin {
        path: "components/cmock/CMock",
        url: "https://github.com/ThrowTheSwitch/CMock/archive/eeecc49ce8af123cf8ad40efdb9673e37b56230f.tar.gz",
        sha256: "b39e8cb357d44630e80800990d731108ffc517ca01632a08d7cc9e3fcd5408f3",
        bytes: 434_529,
    },
    IdfSubmodulePin {
        path: "components/esp_coex/lib",
        url: "https://github.com/espressif/esp-coex-lib/archive/02c57071231dbe9f7555afcbb8ef86e35f517292.tar.gz",
        sha256: "f26eab6fdb3f1e96b3f2ac2640e16a21e8288074b66c4d38828fb9b0ba99a744",
        bytes: 294_117,
    },
    IdfSubmodulePin {
        path: "components/esp_phy/lib",
        url: "https://github.com/espressif/esp-phy-lib/archive/5695f4f38108658bc4a33e4712c1ebcb34911434.tar.gz",
        sha256: "e1619c7b80209c045ba9b265e1a9df5c256b0c57447bc4a5cfe1742503200e60",
        bytes: 3_456_006,
    },
    IdfSubmodulePin {
        path: "components/esp_wifi/lib",
        url: "https://github.com/espressif/esp32-wifi-lib/archive/61bbece9cc8b3a6c8b2b5035f586db5d7abb95d2.tar.gz",
        sha256: "993132bf008ef662ded3b708d32a3a85e750b1ff4f288f5527887bcd6288a8ba",
        bytes: 9_979_774,
    },
    IdfSubmodulePin {
        path: "components/heap/tlsf",
        url: "https://github.com/espressif/tlsf/archive/2867f6883a12920b1969ff9624c0ab0e4185c2ce.tar.gz",
        sha256: "a9dcb699a671b496bfb6a457bf750de25186a6a412f0602c6182ba67f248b536",
        bytes: 15_232,
    },
    IdfSubmodulePin {
        path: "components/json/cJSON",
        url: "https://github.com/DaveGamble/cJSON/archive/b2890c8d76bbb64e710585ebc0a917196b9c67e7.tar.gz",
        sha256: "c3866048d822428ab289d20842c75a39c92c26e967fed2f0706f6f15e6894082",
        bytes: 356_494,
    },
    IdfSubmodulePin {
        path: "components/lwip/lwip",
        url: "https://github.com/espressif/esp-lwip/archive/c6f2f878e7b0f86033214b85547d579be43351e3.tar.gz",
        sha256: "a7db02542a98b796d251645beb3c739289b543edf32468db3d0618b10f0a84f3",
        bytes: 1_641_920,
    },
    IdfSubmodulePin {
        path: "components/mbedtls/mbedtls",
        url: "https://github.com/espressif/mbedtls/archive/2b96dd8eebe880f304c69976b3c2fa0c5100cbb6.tar.gz",
        sha256: "1b3c8ee0d78a6b0b492d1e0732ae11a7451180e010ed4109b5f4f299c6c210a3",
        bytes: 6_729_050,
    },
    IdfSubmodulePin {
        path: "components/mqtt/esp-mqtt",
        url: "https://github.com/espressif/esp-mqtt/archive/0e9ec171b448db2101ea537300976b7ffe595941.tar.gz",
        sha256: "642e16ba821ef45be09c6f14b0d1aec9d606a9faf638a6d80f9a94780c7c6162",
        bytes: 68_648,
    },
    IdfSubmodulePin {
        path: "components/openthread/lib",
        url: "https://github.com/espressif/esp-thread-lib/archive/33209823e559560f54dcd41cbf3dbdd27f46d569.tar.gz",
        sha256: "2ba90d8670965bda665248a36ac5a9b7a9022152bff1989ef47020bf12300761",
        bytes: 929_435,
    },
    IdfSubmodulePin {
        path: "components/openthread/openthread",
        url: "https://github.com/espressif/openthread/archive/43cc05a9bcf780bd758bad53d897e2d88cf8cb75.tar.gz",
        sha256: "4dfccd23e26f5e85d029312ddaae7e6b4d161610bd0ae7ceeaa0c176d393bbf8",
        bytes: 4_487_181,
    },
    IdfSubmodulePin {
        path: "components/protobuf-c/protobuf-c",
        url: "https://github.com/protobuf-c/protobuf-c/archive/abc67a11c6db271bedbb9f58be85d6f4e2ea8389.tar.gz",
        sha256: "6f41b467d7b7242bb201f6466ba433f6eab943e2b6fc53bb753340632c9e45e2",
        bytes: 134_746,
    },
    IdfSubmodulePin {
        path: "components/spiffs/spiffs",
        url: "https://github.com/pellepl/spiffs/archive/ad902cadceb39d0825a97e25ecb5867641f606ba.tar.gz",
        sha256: "8df4810b8b2bd48ed86e764a7231aadc42ffcbc54a3c68649a14a6ebbd428c78",
        bytes: 100_617,
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
        assert_eq!(total, 23, "the pinned release has 23 gitlinks");
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
