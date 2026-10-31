//! Content-addressed reuse of derived ESP32 boot artifacts.
//!
//! Two boot artifacts are produced by spawning an external tool on every
//! build: `bootloader.bin` (esptool `elf2image`, ~300 ms on this host) and
//! `partitions.bin` (`gen_esp32part.py`, ~60 ms). Both are pure functions of
//! inputs that live in the immutable framework package, so a *fresh* project
//! directory — the scenario measured in FastLED/fbuild#1414 — repeats work a
//! previous project already did.
//!
//! The cache key is the producing tool's stamp plus every parameter handed to
//! it, and the cached bytes are the tool's own output, so a hit is
//! byte-identical to a fresh run by construction. An unreadable tool stamp, a
//! missing entry or a failed write all fall back to running the tool; nothing
//! here can make a build fail.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::build_fingerprint::FileStamp;

/// Bump when the cache layout or the key's meaning changes, so a stale entry
/// can never be mistaken for a current one.
const CACHE_SCHEME: &str = "v1";

/// Directory under the fbuild cache root that holds derived boot artifacts.
pub(super) const CACHE_DIR_NAME: &str = "boot-artifacts";

/// Cache key for one derived artifact.
pub(super) struct ArtifactKey {
    digest: String,
}

impl ArtifactKey {
    /// Derive a key from the producing tool and every parameter that changes
    /// the produced bytes. `None` (cache disabled) when the tool cannot be
    /// stamped — e.g. a bare-name tool resolved from the caller's PATH, whose
    /// identity this process cannot prove.
    pub(super) fn new(tool: &Path, parts: &[&str]) -> Option<Self> {
        let stamp = FileStamp::from_path(tool).ok()?;
        let mut hasher = Sha256::new();
        hasher.update(CACHE_SCHEME.as_bytes());
        hasher.update(b"\0");
        hasher.update(
            format!(
                "{}:{}:{}",
                stamp.len, stamp.modified_secs, stamp.modified_nanos
            )
            .as_bytes(),
        );
        for part in parts {
            hasher.update(b"\0");
            hasher.update(part.as_bytes());
        }
        let digest = hasher.finalize();
        let hex: String = digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Some(Self { digest: hex })
    }

    fn path_in(&self, cache_root: &Path) -> PathBuf {
        cache_root
            .join(CACHE_DIR_NAME)
            .join(format!("{}.bin", self.digest))
    }

    /// Copy the cached artifact over `destination`. `false` means miss or an
    /// unusable entry: the caller must run the tool.
    pub(super) fn materialize(&self, cache_root: &Path, destination: &Path) -> bool {
        let cached = self.path_in(cache_root);
        if !cached.exists() {
            return false;
        }
        match std::fs::copy(&cached, destination) {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    path = %cached.display(),
                    %error,
                    "boot artifact cache: could not reuse cached artifact"
                );
                false
            }
        }
    }

    /// Record the tool's freshly produced output. Best effort: a read-only or
    /// full cache root only costs the next build a re-run.
    pub(super) fn store(&self, cache_root: &Path, produced: &Path) {
        let target = self.path_in(cache_root);
        let Some(parent) = target.parent() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(parent) {
            tracing::warn!(path = %parent.display(), %error, "boot artifact cache: create failed");
            return;
        }
        let bytes = match std::fs::read(produced) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(path = %produced.display(), %error, "boot artifact cache: read failed");
                return;
            }
        };
        if let Err(error) = fbuild_core::fs::write_atomic_sync(&target, &bytes) {
            tracing::warn!(path = %target.display(), %error, "boot artifact cache: write failed");
        }
    }
}

/// Hex content hash of a partition CSV, for the `partitions.bin` key: the
/// generated table depends on the CSV's bytes, not on where it lives.
pub(super) fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "boot_artifacts_cache_tests.rs"]
mod tests;
