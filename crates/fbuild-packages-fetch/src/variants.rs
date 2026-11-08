//! Self-contained ESP SDK archive selection; integrity failures never change source.
use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};

use crate::downloader::{
    self,
    manifest::{EntryLookup, MirrorEntry, SliceEntry},
};

const LIBS: &str = "framework-arduinoespressif32-libs";

/// The unified Xtensa toolchain. It is also published as per-chip slices,
/// because `lib/<chip>` is the bulk of the 660 MB tree (FastLED/fbuild#1944).
const XTENSA_TOOLCHAIN: &str = "toolchain-xtensa-esp-elf";

/// The SAME artifact under the name `Esp32Toolchain` gives it
/// (`crates/fbuild-toolchain/src/toolchain/esp32.rs`, `new`/`from_resolved`).
///
/// The shard is looked up by URL, so the name only selects the owner -- but a
/// name the map does not know means "not sliced", silently. Publishing the
/// slices while this alias was missing made every ESP32-S3 build download the
/// whole 176 MB unified archive: the `@esp-toolchain` shard was never even
/// fetched, because the toolchain package is called `esp32-xtensa-gcc`
/// (FastLED/fbuild#1944).
const XTENSA_TOOLCHAIN_PACKAGE: &str = "esp32-xtensa-gcc";

/// Manifest shard that lists the slices for `name`, when the artifact is
/// published as per-chip slices at all.
///
/// The RISC-V toolchain stays whole: its multilib directories are ISA
/// variants (`rv32imc/ilp32`), not chips, so there is nothing chip-shaped to
/// slice. `select` therefore returns the whole bundle for it by construction.
fn slice_shard(name: &str) -> Option<&'static str> {
    match name {
        LIBS => Some("@esp-libs"),
        XTENSA_TOOLCHAIN | XTENSA_TOOLCHAIN_PACKAGE => Some("@esp-toolchain"),
        _ => None,
    }
}

/// A package fetch with an optional board MCU hint. ESP SDK libs and the
/// unified Xtensa toolchain use it.
pub struct FetchRequest<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub url: &'a str,
    pub variant: Option<&'a str>,
}

struct Selection {
    url: String,
    slice: Option<SliceEntry>,
    chip: Option<String>,
    checksum: Option<String>,
}

pub(crate) fn chip_name(mcu: &str) -> String {
    mcu.to_ascii_lowercase()
        .replace('-', "")
        .replacen("esp32_", "esp32", 1)
}

fn select(
    request: &FetchRequest<'_>,
    entry: Option<&MirrorEntry>,
    base: &str,
) -> Result<Selection> {
    let chip = request
        .variant
        .filter(|_| slice_shard(request.name).is_some())
        .map(chip_name);
    if let (Some(chip), Some(entry)) = (&chip, entry) {
        if !entry.yanked {
            if let Some(slice) = entry.slices.as_ref().and_then(|s| s.get(chip)) {
                let url = reqwest::Url::parse(base)
                    .and_then(|b| b.join(&slice.url))
                    .map_err(|e| {
                        FbuildError::PackageError(format!("invalid slice base URL: {e}"))
                    })?;
                return Ok(Selection {
                    url: url.into(),
                    slice: Some(slice.clone()),
                    checksum: Some(slice.sha256.clone()),
                    chip: Some(chip.clone()),
                });
            }
            tracing::info!("no ESP libs slice for {chip}; using mirrored whole bundle");
        }
    }
    Ok(Selection {
        url: entry
            .filter(|e| !e.yanked)
            .and_then(|e| e.urls.first())
            .map_or_else(|| request.url.to_string(), Clone::clone),
        slice: None,
        chip: None,
        checksum: entry.filter(|e| !e.yanked).and_then(|e| e.sha256.clone()),
    })
}

/// A chip slice the mirror publishes for one artifact, with its integrity
/// pins. Exactly what a download needs; no cache layout, no extraction.
#[derive(Debug, Clone)]
pub(crate) struct SliceFetch {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// Look up the published slice for `chip` without downloading it.
///
/// `None` means "use the artifact's own URL": no slice is published for this
/// name, this chip, or this artifact, or the manifest is unavailable. A slice
/// that IS listed is authoritative — a corrupt one is a hard error upstream,
/// not a reason to silently fetch the whole archive.
pub(crate) async fn published_slice(
    name: &str,
    version: &str,
    url: &str,
    chip: &str,
) -> Option<SliceFetch> {
    let shard = slice_shard(name)?;
    if !downloader::manifest_enabled(url) {
        return None;
    }
    let manifest = downloader::published_manifest().await?;
    let entry = match manifest.entry_lookup(url, Some(shard)).await {
        EntryLookup::Found(entry) => Some(entry),
        EntryLookup::NotListed | EntryLookup::Unavailable => None,
    };
    let request = FetchRequest {
        name,
        version,
        url,
        variant: Some(chip),
    };
    let selection = select(
        &request,
        entry.as_deref(),
        manifest.base_url().unwrap_or(""),
    )
    .ok()?;
    let slice = selection.slice?;
    Some(SliceFetch {
        url: selection.url,
        sha256: slice.sha256,
        size: slice.size,
    })
}

fn archive_path(
    root: &Path,
    request: &FetchRequest<'_>,
    chip: Option<&str>,
    url: &str,
) -> NormalizedPath {
    let suffix = chip.map_or_else(String::new, |c| format!("+{c}"));
    let filename_url = if chip.is_none() { request.url } else { url };
    // Include vendor identity so two upstream URLs cannot alias a version.
    let identity = crate::cache::hash_url(request.url);
    NormalizedPath::from(
        root.join(identity)
            .join(format!("{}@{}{suffix}", request.name, request.version))
            .join(filename_url.rsplit('/').next().unwrap_or("archive.tar.xz")),
    )
}

impl FetchRequest<'_> {
    /// Fetch an archive into a variant-separated cache. A cached whole bundle
    /// serves every MCU. Slices are always checked, including cache hits.
    pub async fn fetch(&self, root: &Path) -> Result<NormalizedPath> {
        let whole = archive_path(root, self, None, self.url);
        if whole.is_file() {
            return Ok(whole);
        }
        let manifest = if downloader::manifest_enabled(self.url) {
            downloader::published_manifest().await
        } else {
            None
        };
        let entry = match &manifest {
            Some(m) => match m
                .entry_lookup(
                    self.url,
                    Some(slice_shard(self.name).unwrap_or("@esp-libs")),
                )
                .await
            {
                EntryLookup::Found(e) => Some(e),
                EntryLookup::NotListed | EntryLookup::Unavailable => None,
            },
            None => None,
        };
        let selected = select(
            self,
            entry.as_deref(),
            manifest.as_ref().and_then(|m| m.base_url()).unwrap_or(""),
        )?;
        self.fetch_selected(root, &selected).await
    }

    async fn fetch_selected(&self, root: &Path, selected: &Selection) -> Result<NormalizedPath> {
        let path = archive_path(root, self, selected.chip.as_deref(), &selected.url);
        if path.is_file() {
            verify(&path, selected.slice.as_ref())?;
            return Ok(path);
        }
        let parent = path
            .parent()
            .ok_or_else(|| FbuildError::PackageError("archive has no parent".into()))?;
        std::fs::create_dir_all(parent)?;
        // Download in scratch, verify before publishing a cache hit. Direct URL
        // retries are permitted, whole-bundle fallback after slice failure is not.
        let scratch = tempfile::tempdir_in(parent)?;
        let downloaded = if selected.slice.is_some() {
            let response = crate::http::client()
                .get(&selected.url)
                .send()
                .await
                .map_err(|e| FbuildError::PackageError(e.to_string()))?
                .error_for_status()
                .map_err(|e| FbuildError::PackageError(e.to_string()))?;
            let file = scratch.path().join("slice.tar.zst");
            let bytes = response
                .bytes()
                .await
                .map_err(|e| FbuildError::PackageError(e.to_string()))?;
            std::fs::write(&file, bytes)?;
            file
        } else {
            downloader::download_file(&selected.url, scratch.path()).await?
        };
        verify(&downloaded, selected.slice.as_ref())?;
        if let Some(checksum) = &selected.checksum {
            downloader::verify_checksum(&downloaded, checksum)?;
        }
        std::fs::rename(downloaded, &path)?;
        Ok(path)
    }
}

fn verify(path: &Path, slice: Option<&SliceEntry>) -> Result<()> {
    if let Some(slice) = slice {
        if std::fs::metadata(path)?.len() != slice.size {
            return Err(FbuildError::PackageError(format!(
                "slice size mismatch: {}",
                path.display()
            )));
        }
        downloader::verify_checksum(path, &slice.sha256)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::manifest::MirrorManifest;
    use sha2::{Digest, Sha256};

    const VENDOR: &str = "https://vendor.example/esp32-3.3.5-libs.tar.xz";
    const BASE: &str = "https://bucket.example/manifest.json";

    fn request(chip: &str) -> FetchRequest<'_> {
        FetchRequest {
            name: LIBS,
            version: "3.3.5",
            url: VENDOR,
            variant: Some(chip),
        }
    }

    fn manifest(with_slices: bool) -> MirrorManifest {
        let mut asset = serde_json::json!({"urls": ["https://mirror.example/whole.tar.xz", VENDOR], "sha256": "a".repeat(64)});
        if with_slices {
            asset["slices"] = serde_json::json!({"esp32s3": {"url": "slices/s3.tar.zst", "size": 3, "sha256": "b".repeat(64), "format": "tar.zst"}});
        }
        MirrorManifest::parse(&serde_json::to_vec(&serde_json::json!({"kind":"Release", "schema_version":1,"components":[{"asset":asset}]})).unwrap()).unwrap()
    }

    /// The toolchain slice has to be selected for the SAME chip token the
    /// slicer names its archives with, and never for a chip it has no slice
    /// for -- silently using another chip's compiler tree fails at link.
    #[tokio::test]
    async fn toolchain_slices_are_chip_scoped_and_xtensa_only() {
        let mut asset = serde_json::json!({"urls": ["https://mirror.example/tc.tar.gz", VENDOR]});
        asset["slices"] = serde_json::json!({
            "esp32s3": {"url": "slices/tc-esp32s3.tar.zst", "size": 3, "sha256": "c".repeat(64), "format": "tar.zst"}
        });
        let manifest = MirrorManifest::parse(
            &serde_json::to_vec(&serde_json::json!({
                "kind": "Release", "schema_version": 1,
                "components": [{"asset": asset}]
            }))
            .unwrap(),
        )
        .unwrap();
        let entry = match manifest.entry_lookup(VENDOR, None).await {
            EntryLookup::Found(entry) => entry,
            _ => panic!("missing entry"),
        };

        let toolchain = |chip: &'static str| FetchRequest {
            name: "toolchain-xtensa-esp-elf",
            version: "14.2.0_20241119",
            url: VENDOR,
            variant: Some(chip),
        };
        let sliced = select(&toolchain("ESP32-S3"), Some(&entry), BASE).unwrap();
        assert_eq!(
            sliced.url,
            "https://bucket.example/slices/tc-esp32s3.tar.zst"
        );
        assert_eq!(sliced.chip.as_deref(), Some("esp32s3"));
        // No slice for this chip: the whole archive is the fallback -- the
        // mirrored copy when the entry names one, else the vendor URL.
        let whole = select(&toolchain("esp32c2"), Some(&entry), BASE).unwrap();
        assert_eq!(whole.url, "https://mirror.example/tc.tar.gz");
        assert!(whole.slice.is_none());
        assert_eq!(
            select(&toolchain("esp32c2"), None, BASE).unwrap().url,
            VENDOR
        );
        // RISC-V multilibs are ISA-shaped, not chip-shaped: never sliced.
        let riscv = FetchRequest {
            name: "toolchain-riscv32-esp",
            ..toolchain("esp32c3")
        };
        assert!(select(&riscv, Some(&entry), BASE).unwrap().slice.is_none());
        assert_eq!(slice_shard(LIBS), Some("@esp-libs"));
        assert_eq!(
            slice_shard("toolchain-xtensa-esp-elf"),
            Some("@esp-toolchain")
        );
        assert_eq!(slice_shard("toolchain-riscv32-esp"), None);
        assert_eq!(slice_shard("platform-espressif32"), None);
    }

    /// Both names this artifact is constructed under must reach the shard.
    ///
    /// `esp32-xtensa-gcc` is what `Esp32Toolchain::new`/`from_resolved` pass to
    /// `PackageBase` in `crates/fbuild-toolchain/src/toolchain/esp32.rs`; the
    /// manifest name is what the slicer/publisher use. Looking the shard up by
    /// URL hides the difference until a real build silently downloads the whole
    /// archive (FastLED/fbuild#1944).
    #[test]
    fn every_name_the_esp_orchestrator_builds_resolves_to_the_toolchain_shard() {
        for name in [XTENSA_TOOLCHAIN, "esp32-xtensa-gcc"] {
            assert_eq!(slice_shard(name), Some("@esp-toolchain"), "{name}");
        }
        // The RISC-V toolchain keeps the whole archive under either of its
        // names: its multilibs are ISA variants, not chips.
        for name in ["toolchain-riscv32-esp", "esp32-riscv-gcc"] {
            assert_eq!(slice_shard(name), None, "{name}");
        }
    }

    #[test]
    fn mcu_normalization_preserves_engineering_sample_directory_suffix() {
        for (mcu, chip) in [
            ("ESP32-S3", "esp32s3"),
            ("esp32_s3", "esp32s3"),
            ("esp32p4_es", "esp32p4_es"),
        ] {
            assert_eq!(chip_name(mcu), chip);
        }
    }

    #[tokio::test]
    async fn old_and_new_manifests_selection_matrix_and_relative_bases() {
        let old = manifest(false);
        let new = manifest(true);
        let entry = match new.entry_lookup(VENDOR, None).await {
            EntryLookup::Found(e) => e,
            _ => panic!("missing entry"),
        };
        assert_eq!(select(&request("esp32s3"), None, BASE).unwrap().url, VENDOR);
        let old_entry = match old.entry_lookup(VENDOR, None).await {
            EntryLookup::Found(e) => e,
            _ => panic!("missing entry"),
        };
        assert!(old_entry.slices.is_none());
        assert_eq!(
            select(&request("esp32c3"), Some(&entry), BASE).unwrap().url,
            "https://mirror.example/whole.tar.xz"
        );
        for (base, expected) in [
            (BASE, "https://bucket.example/slices/s3.tar.zst"),
            (
                "https://fastled.github.io/fbuild/mirror/manifest.json",
                "https://fastled.github.io/fbuild/mirror/slices/s3.tar.zst",
            ),
        ] {
            let selection = select(&request("ESP32-S3"), Some(&entry), base).unwrap();
            assert_eq!(selection.url, expected);
            assert_eq!(selection.chip.as_deref(), Some("esp32s3"));
        }
        let mut other = request("esp32s3");
        other.name = "toolchain";
        assert!(select(&other, Some(&entry), BASE).unwrap().slice.is_none());
    }

    #[tokio::test]
    async fn slice_cache_is_chip_specific_and_whole_serves_both() {
        let tmp = tempfile::tempdir().unwrap();
        let s3 = request("esp32s3");
        let c3 = request("esp32c3");
        let slice = archive_path(tmp.path(), &s3, Some("esp32s3"), "s3.tar.zst");
        std::fs::create_dir_all(slice.parent().unwrap()).unwrap();
        std::fs::write(&slice, b"slice").unwrap();
        assert!(!archive_path(tmp.path(), &c3, Some("esp32c3"), "c3.tar.zst").exists());
        for request in [&s3, &c3] {
            assert!(!archive_path(tmp.path(), request, None, VENDOR).exists());
        }
        let whole = archive_path(tmp.path(), &s3, None, VENDOR);
        assert_eq!(
            whole,
            archive_path(
                tmp.path(),
                &s3,
                None,
                "https://mirror.example/renamed.tar.xz"
            )
        );
        std::fs::create_dir_all(whole.parent().unwrap()).unwrap();
        std::fs::write(&whole, b"whole").unwrap();
        assert_eq!(whole, archive_path(tmp.path(), &c3, None, VENDOR));
        assert_eq!(s3.fetch(tmp.path()).await.unwrap(), whole);
        assert_eq!(c3.fetch(tmp.path()).await.unwrap(), whole);
    }

    #[tokio::test]
    async fn corrupt_slice_is_hard_error_without_bundle_request() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 2048];
            let n = stream.read(&mut buffer).unwrap();
            assert!(String::from_utf8_lossy(&buffer[..n]).starts_with("GET /slice "));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad")
                .unwrap();
            listener.set_nonblocking(true).unwrap();
            // A bundle request would already be queued before fetch returns;
            // the test joins only after that return, below.
            listener
        });
        let tmp = tempfile::tempdir().unwrap();
        let selected = Selection {
            url: format!("http://{addr}/slice"),
            chip: Some("esp32s3".into()),
            checksum: None,
            slice: Some(SliceEntry {
                url: "slice".into(),
                sha256: format!("{:x}", Sha256::digest(b"ok!")),
                size: 3,
                format: "tar.zst".into(),
            }),
        };
        let error = request("esp32s3")
            .fetch_selected(tmp.path(), &selected)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"), "{error}");
        let listener = server.join().unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let path = archive_path(
            tmp.path(),
            &request("esp32s3"),
            Some("esp32s3"),
            &selected.url,
        );
        assert!(!path.exists());
    }

    #[test]
    fn cached_slice_size_and_digest_are_verified() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("slice");
        std::fs::write(&path, b"abc").unwrap();
        let mut slice = SliceEntry {
            url: "slice".into(),
            sha256: format!("{:x}", Sha256::digest(b"abc")),
            size: 3,
            format: "tar.zst".into(),
        };
        verify(&path, Some(&slice)).unwrap();
        slice.size = 4;
        assert!(
            verify(&path, Some(&slice))
                .unwrap_err()
                .to_string()
                .contains("size mismatch")
        );
        slice.size = 3;
        slice.sha256 = "0".repeat(64);
        assert!(
            verify(&path, Some(&slice))
                .unwrap_err()
                .to_string()
                .contains("checksum mismatch")
        );
    }
}
