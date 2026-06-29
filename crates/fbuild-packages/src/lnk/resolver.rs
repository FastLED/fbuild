//! Resolve a `LnkFile` to an on-disk blob path, fetching + caching as needed.
//!
//! Cache layer: uses the existing `DiskCache` with `Kind::LnkBlobs`. The
//! cache key triple is `(LnkBlobs, lnk.url, lnk.sha256)` — we use the
//! sha256 in the "version" slot so identical content under different URLs
//! still produces a deterministic, content-addressable layout per URL,
//! while sharing the LRU + lease infrastructure that all other cache kinds
//! get for free.
//!
//! On cache hit: return the cached blob path + a `Lease` that pins it
//! against GC for the duration of the build.
//!
//! On cache miss: download via the existing `downloader`, verify the
//! sha256, write to the cache's archive directory, record the entry,
//! and return the lease + path.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use fbuild_core::{FbuildError, Result};
use sha2::{Digest, Sha256};
use tokio::runtime::Runtime;
use tracing::{debug, info};

use super::format::LnkFile;
use crate::disk_cache::{CacheEntry, Kind, Lease};
use crate::downloader::download_file;
use crate::DiskCache;

/// Module-level fallback runtime for the sync `.lnk` resolver bridge.
///
/// See [`crate::library::library_manager::fallback_runtime`] for the same
/// pattern — building a Tokio runtime per call is expensive and pointless
/// when this module gets hit repeatedly during a build.
fn fallback_runtime() -> Result<&'static Runtime> {
    static RT: OnceLock<Runtime> = OnceLock::new();
    if let Some(rt) = RT.get() {
        return Ok(rt);
    }
    let rt = Runtime::new().map_err(|e| {
        FbuildError::PackageError(format!("failed to create tokio runtime: {}", e))
    })?;
    Ok(RT.get_or_init(|| rt))
}

/// A successfully resolved `.lnk` blob. Holds a `Lease` that keeps the
/// blob pinned in the cache; the lease drops when this struct does.
///
/// `Debug` is implemented manually because `Lease` is intentionally not
/// `Debug` (it carries a SQLite handle).
pub struct ResolvedBlob {
    /// On-disk path to the resolved blob (absolute).
    pub path: PathBuf,
    /// SHA-256 of the blob (matches `LnkFile::sha256`).
    pub sha256: String,
    /// The cache entry record, if a `DiskCache` was supplied.
    pub entry: Option<CacheEntry>,
    /// Lease that pins the entry; drop this to release the lease.
    pub lease: Option<Lease>,
}

impl std::fmt::Debug for ResolvedBlob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedBlob")
            .field("path", &self.path)
            .field("sha256", &self.sha256)
            .field("entry_id", &self.entry.as_ref().map(|e| e.id))
            .field("has_lease", &self.lease.is_some())
            .finish()
    }
}

/// Resolve a `.lnk` file: cache hit → return cached path + lease;
/// cache miss → download, verify, record, return path + lease.
///
/// The download path runs synchronously by blocking on the existing async
/// downloader. Callers already on a tokio runtime get `block_in_place`;
/// off-runtime callers get a fresh single-thread runtime.
pub fn resolve(lnk: &LnkFile, cache: &DiskCache) -> Result<ResolvedBlob> {
    // Cache lookup uses (Kind, url, version) where "version" is the sha256.
    // This guarantees that a change to the .lnk's sha256 forces a refetch.
    if let Some(entry) = cache
        .lookup(Kind::LnkBlobs, &lnk.url, &lnk.sha256)
        .map_err(map_cache_err)?
    {
        // Verify the blob is still on disk (the index can outlive a manual
        // cache wipe) and matches the expected sha256.
        let blob_path = blob_path_for(&entry);
        if blob_path.exists() {
            // Best-effort sha verify on cache hit — cheap (single read,
            // not network). Catches accidental cache corruption.
            if verify_sha256(&blob_path, &lnk.sha256).is_ok() {
                // Pin the entry against concurrent GC for the lifetime of
                // the `ResolvedBlob`. Schema-migration guarantees the
                // `leases.refcount` column exists even on old caches
                // (see `disk_cache::index::MIGRATIONS`).
                let lease = Some(cache.lease(&entry).map_err(map_cache_err)?);
                let _ = cache.touch(&entry);
                debug!(url = %lnk.url, sha = %lnk.sha256, "lnk cache hit");
                return Ok(ResolvedBlob {
                    path: blob_path,
                    sha256: lnk.sha256.clone(),
                    entry: Some(entry),
                    lease,
                });
            }
            tracing::warn!(
                path = %blob_path.display(),
                "cached lnk blob failed sha verify; refetching"
            );
        } else {
            tracing::warn!(
                path = %blob_path.display(),
                "cached lnk blob missing on disk; refetching"
            );
        }
    }

    // Cache miss → fetch.
    debug!(url = %lnk.url, "lnk cache miss; fetching");

    // Stage into the per-entry archive dir. The cache's path helpers give
    // us a stable, sanitized location keyed on (kind, url, version).
    let staging_dir = cache.archive_staging_dir(Kind::LnkBlobs, &lnk.url, &lnk.sha256);
    let archive_dir = cache.archive_dir(Kind::LnkBlobs, &lnk.url, &lnk.sha256);
    std::fs::create_dir_all(&staging_dir).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to create lnk staging dir {}: {e}",
            staging_dir.display()
        ))
    })?;

    let downloaded = {
        let fut = async { download_file(&lnk.url, &staging_dir).await };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            tokio::task::block_in_place(|| handle.block_on(fut))?
        } else {
            fallback_runtime()?.block_on(fut)?
        }
    };

    verify_sha256(&downloaded, &lnk.sha256).map_err(|e| {
        // Clean up the staging file so a retry starts fresh.
        let _ = std::fs::remove_file(&downloaded);
        e
    })?;

    let archive_bytes = std::fs::metadata(&downloaded)
        .map(|m| m.len() as i64)
        .unwrap_or(0);

    // Promote staging → archive.
    std::fs::create_dir_all(&archive_dir).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to create lnk archive dir {}: {e}",
            archive_dir.display()
        ))
    })?;
    let final_path = archive_dir.join(
        downloaded
            .file_name()
            .ok_or_else(|| FbuildError::PackageError("downloaded file has no name".to_string()))?,
    );
    if final_path.exists() {
        let _ = std::fs::remove_file(&final_path);
    }
    std::fs::rename(&downloaded, &final_path).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to move lnk blob {} → {}: {e}",
            downloaded.display(),
            final_path.display()
        ))
    })?;

    let entry = cache
        .record_archive(
            Kind::LnkBlobs,
            &lnk.url,
            &lnk.sha256,
            &final_path.to_string_lossy(),
            archive_bytes,
            &lnk.sha256,
        )
        .map_err(map_cache_err)?;

    let lease = Some(cache.lease(&entry).map_err(map_cache_err)?);
    info!(
        url = %lnk.url,
        bytes = archive_bytes,
        path = %final_path.display(),
        "lnk blob fetched and cached"
    );

    Ok(ResolvedBlob {
        path: final_path,
        sha256: lnk.sha256.clone(),
        entry: Some(entry),
        lease,
    })
}

/// Reconstruct the on-disk blob path from a `CacheEntry`. The entry's
/// `archive_path` is set when `record_archive` was called.
fn blob_path_for(entry: &CacheEntry) -> PathBuf {
    PathBuf::from(entry.archive_path.clone().unwrap_or_default())
}

fn map_cache_err(e: rusqlite::Error) -> FbuildError {
    FbuildError::PackageError(format!("lnk cache index error: {e}"))
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let bytes = std::fs::read(path).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to read {} for sha verify: {e}",
            path.display()
        ))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected.to_ascii_lowercase() {
        return Err(FbuildError::PackageError(format!(
            "sha256 mismatch for {}: expected {expected}, got {actual}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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

    #[test]
    fn verify_sha256_matches() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.bin");
        let bytes = b"hello world";
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        verify_sha256(&p, &sha256_of(bytes)).unwrap();
    }

    #[test]
    fn verify_sha256_mismatch_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.bin");
        std::fs::write(&p, b"actual content").unwrap();
        let bogus = "0".repeat(64);
        let err = verify_sha256(&p, &bogus).unwrap_err().to_string();
        assert!(err.contains("sha256 mismatch"), "got: {err}");
    }

    /// Cache-hit path: pre-populate the cache with a blob whose sha matches,
    /// resolve(), assert no network was needed.
    ///
    /// We exercise this by manually staging a file through the disk_cache
    /// API, then calling resolve() which should short-circuit on the hit.
    #[test]
    fn resolve_returns_cache_hit_without_network() {
        let (_tmp, cache) = open_test_cache();
        let blob_bytes = b"cached content";
        let sha = sha256_of(blob_bytes);

        // Stage the blob into the cache's archive layout manually.
        let url = "https://localhost.invalid/never-fetched.bin";
        let archive_dir = cache.archive_dir(Kind::LnkBlobs, url, &sha);
        std::fs::create_dir_all(&archive_dir).unwrap();
        let blob_path = archive_dir.join("never-fetched.bin");
        std::fs::write(&blob_path, blob_bytes).unwrap();

        let _entry = cache
            .record_archive(
                Kind::LnkBlobs,
                url,
                &sha,
                &blob_path.to_string_lossy(),
                blob_bytes.len() as i64,
                &sha,
            )
            .unwrap();

        let lnk = LnkFile {
            version: 1,
            url: url.to_string(),
            sha256: sha.clone(),
            size: None,
            extract: super::super::ExtractMode::File,
        };

        // localhost.invalid never resolves — if resolve() tried to network,
        // this would fail. Cache hit means no network.
        let resolved = resolve(&lnk, &cache).unwrap();
        assert_eq!(resolved.path, blob_path);
        assert_eq!(resolved.sha256, sha);
        assert!(resolved.lease.is_some());
    }

    /// Cache-hit but stored sha doesn't match content → resolve must fall
    /// through to refetch (which then fails because we used a fake URL,
    /// but the *behavior* we care about is that the bad cache was rejected).
    #[test]
    fn resolve_rejects_corrupt_cache_entry() {
        let (_tmp, cache) = open_test_cache();
        let url = "https://localhost.invalid/corrupt.bin";
        let claimed_sha = sha256_of(b"good content"); // what .lnk says
        let archive_dir = cache.archive_dir(Kind::LnkBlobs, url, &claimed_sha);
        std::fs::create_dir_all(&archive_dir).unwrap();
        let blob_path = archive_dir.join("corrupt.bin");
        // But on-disk content is wrong.
        std::fs::write(&blob_path, b"corrupt actual content").unwrap();
        cache
            .record_archive(
                Kind::LnkBlobs,
                url,
                &claimed_sha,
                &blob_path.to_string_lossy(),
                100,
                &claimed_sha,
            )
            .unwrap();

        let lnk = LnkFile {
            version: 1,
            url: url.to_string(),
            sha256: claimed_sha,
            size: None,
            extract: super::super::ExtractMode::File,
        };

        // Should attempt to refetch (and fail because URL is bogus).
        // The interesting assertion: it didn't silently return the corrupt blob.
        let result = resolve(&lnk, &cache);
        assert!(result.is_err(), "expected refetch failure, got Ok");
    }
}
