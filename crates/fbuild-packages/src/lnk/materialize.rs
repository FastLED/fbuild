//! Materialize resolved `.lnk` blobs into a build-tree directory.
//!
//! Source-tree `.lnk` files at `<src>/path/to/foo.ext.lnk` are projected
//! into `<build_resources_dir>/path/to/foo.ext`. This keeps the source
//! tree clean (no .gitignore wildcards) while presenting downstream
//! build steps with normal-looking files.
//!
//! For `extract: "file"` (default) the cached blob is hardlinked (or
//! copied as a fallback) to the target path. For `"zip"` and `"tar.gz"`
//! the cached blob is extracted into a directory at the target path.

use std::path::{Path, PathBuf};

use fbuild_core::{FbuildError, Result};
use tracing::debug;

use super::format::{ExtractMode, LnkFile};
use super::resolver::{resolve, ResolvedBlob};
use super::scanner::DiscoveredLnk;
use crate::extractor::{extract_tar_gz_public, extract_zip_public};
use crate::DiskCache;

/// One materialized `.lnk` ready for downstream consumers.
pub struct MaterializedLnk {
    /// Source-tree path of the `.lnk` file (the pointer, not the data).
    pub lnk_path: PathBuf,
    /// Where the blob now lives in the build tree (file or directory).
    pub target_path: PathBuf,
    /// SHA-256 of the source blob.
    pub sha256: String,
    /// Resolution result, including the cache lease.
    /// Held in this struct so the lease lives at least as long as the
    /// caller's reference to the materialized output.
    pub resolved: ResolvedBlob,
}

impl std::fmt::Debug for MaterializedLnk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaterializedLnk")
            .field("lnk_path", &self.lnk_path)
            .field("target_path", &self.target_path)
            .field("sha256", &self.sha256)
            .finish()
    }
}

/// Materialize a single `.lnk`.
///
/// The caller specifies:
/// - `lnk_path` — absolute path to the `.lnk` file
/// - `lnk` — parsed contents
/// - `target_path` — where the resolved blob (or extracted tree) should land
/// - `cache` — disk cache used for fetch+lookup
pub fn materialize_one(
    lnk_path: &Path,
    lnk: &LnkFile,
    target_path: &Path,
    cache: &DiskCache,
) -> Result<MaterializedLnk> {
    let resolved = resolve(lnk, cache)?;

    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            FbuildError::PackageError(format!(
                "failed to create target dir {}: {e}",
                parent.display()
            ))
        })?;
    }

    match lnk.extract {
        ExtractMode::File => {
            place_file(&resolved.path, target_path)?;
        }
        ExtractMode::Zip => {
            // Replace any pre-existing tree at target before extracting.
            if target_path.exists() {
                let meta = std::fs::symlink_metadata(target_path).map_err(|e| {
                    FbuildError::PackageError(format!(
                        "failed to stat existing target {}: {e}",
                        target_path.display()
                    ))
                })?;
                if meta.is_dir() {
                    std::fs::remove_dir_all(target_path).map_err(|e| {
                        FbuildError::PackageError(format!(
                            "failed to clear target dir {}: {e}",
                            target_path.display()
                        ))
                    })?;
                } else {
                    std::fs::remove_file(target_path).ok();
                }
            }
            std::fs::create_dir_all(target_path).map_err(|e| {
                FbuildError::PackageError(format!(
                    "failed to create extract target {}: {e}",
                    target_path.display()
                ))
            })?;
            extract_zip_public(&resolved.path, target_path)?;
        }
        ExtractMode::TarGz => {
            if target_path.exists() {
                let meta = std::fs::symlink_metadata(target_path).map_err(|e| {
                    FbuildError::PackageError(format!(
                        "failed to stat existing target {}: {e}",
                        target_path.display()
                    ))
                })?;
                if meta.is_dir() {
                    std::fs::remove_dir_all(target_path).map_err(|e| {
                        FbuildError::PackageError(format!(
                            "failed to clear target dir {}: {e}",
                            target_path.display()
                        ))
                    })?;
                } else {
                    std::fs::remove_file(target_path).ok();
                }
            }
            std::fs::create_dir_all(target_path).map_err(|e| {
                FbuildError::PackageError(format!(
                    "failed to create extract target {}: {e}",
                    target_path.display()
                ))
            })?;
            extract_tar_gz_public(&resolved.path, target_path)?;
        }
    }

    debug!(
        lnk = %lnk_path.display(),
        target = %target_path.display(),
        sha = %resolved.sha256,
        "materialized lnk"
    );

    Ok(MaterializedLnk {
        lnk_path: lnk_path.to_path_buf(),
        target_path: target_path.to_path_buf(),
        sha256: resolved.sha256.clone(),
        resolved,
    })
}

/// Materialize every `DiscoveredLnk` into a target tree under
/// `build_resources_dir`. Each `.lnk` at `<source_root>/<rel>/foo.ext.lnk`
/// is materialized to `<build_resources_dir>/<rel>/foo.ext`.
pub fn materialize_all(
    discovered: &[DiscoveredLnk],
    source_root: &Path,
    build_resources_dir: &Path,
    cache: &DiskCache,
) -> Result<Vec<MaterializedLnk>> {
    let mut out = Vec::with_capacity(discovered.len());
    for d in discovered {
        let target = target_path_for(&d.path, source_root, build_resources_dir)?;
        out.push(materialize_one(&d.path, &d.lnk, &target, cache)?);
    }
    Ok(out)
}

/// Compute the materialized target path for a `.lnk` at `lnk_path`.
/// Strips the `.lnk` suffix and rebases under `build_resources_dir`
/// preserving the relative position from `source_root`.
fn target_path_for(
    lnk_path: &Path,
    source_root: &Path,
    build_resources_dir: &Path,
) -> Result<PathBuf> {
    let rel = lnk_path.strip_prefix(source_root).map_err(|_| {
        FbuildError::PackageError(format!(
            "lnk path {} is not under source root {}",
            lnk_path.display(),
            source_root.display()
        ))
    })?;
    let stripped = strip_lnk_suffix(rel)?;
    Ok(build_resources_dir.join(stripped))
}

fn strip_lnk_suffix(rel: &Path) -> Result<PathBuf> {
    let file_name = rel.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        FbuildError::PackageError(format!("cannot decode lnk file name: {}", rel.display()))
    })?;
    let stripped = file_name.strip_suffix(".lnk").ok_or_else(|| {
        FbuildError::PackageError(format!("lnk path does not end in .lnk: {}", rel.display()))
    })?;
    Ok(rel
        .parent()
        .map(|p| p.join(stripped))
        .unwrap_or_else(|| PathBuf::from(stripped)))
}

/// Hardlink the cached blob into place; if hardlink fails (e.g. cross-device,
/// platform doesn't support it), fall back to a regular copy.
fn place_file(src: &Path, dst: &Path) -> Result<()> {
    // Remove any existing target — replacing keeps semantics deterministic
    // (a stale leftover from a prior build won't shadow the new blob).
    if dst.exists() || dst.symlink_metadata().is_ok() {
        std::fs::remove_file(dst).map_err(|e| {
            FbuildError::PackageError(format!(
                "failed to remove existing target {}: {e}",
                dst.display()
            ))
        })?;
    }
    if std::fs::hard_link(src, dst).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dst).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to copy lnk blob {} → {}: {e}",
            src.display(),
            dst.display()
        ))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn sha256_of(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    }

    fn open_test_cache() -> (tempfile::TempDir, DiskCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::open_at(dir.path()).unwrap();
        (dir, cache)
    }

    /// Pre-stage a blob in the cache so resolve() takes the cache-hit path
    /// (no network needed in tests).
    fn stage_in_cache(cache: &DiskCache, url: &str, sha: &str, bytes: &[u8]) -> PathBuf {
        let dir = cache.archive_dir(crate::disk_cache::Kind::LnkBlobs, url, sha);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("blob");
        std::fs::write(&p, bytes).unwrap();
        cache
            .record_archive(
                crate::disk_cache::Kind::LnkBlobs,
                url,
                sha,
                &p.to_string_lossy(),
                bytes.len() as i64,
                sha,
            )
            .unwrap();
        p
    }

    #[test]
    fn target_path_strips_lnk_suffix_preserving_relative() {
        let src_root = Path::new("/repo/src");
        let lnk_path = Path::new("/repo/src/assets/sample.bin.lnk");
        let build = Path::new("/repo/.build/resources");
        let target = target_path_for(lnk_path, src_root, build).unwrap();
        assert_eq!(
            target,
            Path::new("/repo/.build/resources/assets/sample.bin")
        );
    }

    #[test]
    fn target_path_top_level_lnk() {
        let src_root = Path::new("/repo");
        let lnk_path = Path::new("/repo/foo.bin.lnk");
        let build = Path::new("/build");
        let target = target_path_for(lnk_path, src_root, build).unwrap();
        assert_eq!(target, Path::new("/build/foo.bin"));
    }

    #[test]
    fn target_path_rejects_non_lnk_suffix() {
        let err = target_path_for(
            Path::new("/repo/foo.bin"),
            Path::new("/repo"),
            Path::new("/build"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("does not end in .lnk"), "got: {err}");
    }

    #[test]
    fn target_path_rejects_lnk_outside_source_root() {
        let err = target_path_for(
            Path::new("/elsewhere/foo.bin.lnk"),
            Path::new("/repo"),
            Path::new("/build"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not under source root"), "got: {err}");
    }

    #[test]
    fn materialize_file_mode_creates_target() {
        let (_tmp, cache) = open_test_cache();
        let bytes = b"materialized content";
        let sha = sha256_of(bytes);
        let url = "https://localhost.invalid/x.bin";
        stage_in_cache(&cache, url, &sha, bytes);

        let lnk = LnkFile {
            version: 1,
            url: url.to_string(),
            sha256: sha.clone(),
            size: None,
            extract: ExtractMode::File,
        };
        let work = tempfile::tempdir().unwrap();
        let lnk_path = work.path().join("src/foo.bin.lnk");
        let target = work.path().join("build/foo.bin");

        let m = materialize_one(&lnk_path, &lnk, &target, &cache).unwrap();
        let got = std::fs::read(&m.target_path).unwrap();
        assert_eq!(got, bytes);
        assert_eq!(m.sha256, sha);
    }

    #[test]
    fn materialize_replaces_existing_target() {
        let (_tmp, cache) = open_test_cache();
        let bytes = b"new bytes";
        let sha = sha256_of(bytes);
        let url = "https://localhost.invalid/y.bin";
        stage_in_cache(&cache, url, &sha, bytes);

        let lnk = LnkFile {
            version: 1,
            url: url.to_string(),
            sha256: sha.clone(),
            size: None,
            extract: ExtractMode::File,
        };
        let work = tempfile::tempdir().unwrap();
        let target = work.path().join("foo.bin");
        std::fs::write(&target, b"stale").unwrap();
        let lnk_path = work.path().join("foo.bin.lnk");

        materialize_one(&lnk_path, &lnk, &target, &cache).unwrap();
        let got = std::fs::read(&target).unwrap();
        assert_eq!(got, bytes);
    }

    #[test]
    fn materialize_zip_extracts_into_directory() {
        // Build a tiny in-memory zip with one entry.
        let (_tmp, cache) = open_test_cache();
        let zip_bytes = make_zip_with_entry("hello.txt", b"hi from zip");
        let sha = sha256_of(&zip_bytes);
        let url = "https://localhost.invalid/x.zip";
        stage_in_cache(&cache, url, &sha, &zip_bytes);

        let lnk = LnkFile {
            version: 1,
            url: url.to_string(),
            sha256: sha,
            size: None,
            extract: ExtractMode::Zip,
        };
        let work = tempfile::tempdir().unwrap();
        let lnk_path = work.path().join("foo.zip.lnk");
        let target = work.path().join("build/foo.zip");
        materialize_one(&lnk_path, &lnk, &target, &cache).unwrap();
        let extracted = std::fs::read(target.join("hello.txt")).unwrap();
        assert_eq!(extracted, b"hi from zip");
    }

    #[test]
    fn materialize_all_walks_tree() {
        let (_tmp, cache) = open_test_cache();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("src");
        let build = work.path().join("build/resources");

        let bytes_a = b"file a";
        let sha_a = sha256_of(bytes_a);
        stage_in_cache(&cache, "https://x/a.bin", &sha_a, bytes_a);
        let bytes_b = b"file bee";
        let sha_b = sha256_of(bytes_b);
        stage_in_cache(&cache, "https://x/b.bin", &sha_b, bytes_b);

        let path_a = src.join("nested/a.bin.lnk");
        let path_b = src.join("b.bin.lnk");
        std::fs::create_dir_all(path_a.parent().unwrap()).unwrap();
        std::fs::write(
            &path_a,
            format!(r#"{{"v":1,"url":"https://x/a.bin","sha256":"{sha_a}"}}"#),
        )
        .unwrap();
        std::fs::write(
            &path_b,
            format!(r#"{{"v":1,"url":"https://x/b.bin","sha256":"{sha_b}"}}"#),
        )
        .unwrap();

        let discovered = super::super::scanner::scan_for_lnk(&src).unwrap();
        let materialized = materialize_all(&discovered, &src, &build, &cache).unwrap();
        assert_eq!(materialized.len(), 2);
        assert_eq!(std::fs::read(build.join("nested/a.bin")).unwrap(), bytes_a);
        assert_eq!(std::fs::read(build.join("b.bin")).unwrap(), bytes_b);
    }

    /// Minimal zip builder for tests — one entry, no compression.
    fn make_zip_with_entry(name: &str, contents: &[u8]) -> Vec<u8> {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;
        use zip::CompressionMethod;
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            w.start_file(name, opts).unwrap();
            w.write_all(contents).unwrap();
            w.finish().unwrap();
        }
        buf.into_inner()
    }
}
