//! Self-contained ESP SDK archive selection; integrity failures never change source.
use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};

use crate::downloader::{
    self,
    manifest::{EntryLookup, MirrorEntry, SliceEntry},
};

const LIBS: &str = "framework-arduinoespressif32-libs";

/// A package fetch with an optional board MCU hint. Only ESP SDK libs use it.
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

fn chip_name(mcu: &str) -> String {
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
        .filter(|_| request.name == LIBS)
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
            Some(m) => match m.entry_lookup(self.url, Some("@esp-libs")).await {
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
