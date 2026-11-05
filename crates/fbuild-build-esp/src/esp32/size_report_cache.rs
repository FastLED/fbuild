//! Cross-project reuse of the ESP32 `size` report (FastLED/fbuild#1894).
//!
//! A fresh project directory no longer relinks (`link_output_cache`, #1900) or
//! re-runs `esptool elf2image` (`firmware_bin_cache`, #1902), but it still
//! spawned the size tool on the restored ELF: **213 ms of a 1216 ms** fresh-dir
//! build (probe 2026-11-04, ESP32-S3 Blink), the last remaining subprocess on
//! the post-link path. `size` only reads the ELF, so its report is a pure
//! function of the linked bytes — the same bytes a cached ELF restores.
//!
//! Reuse is strictly best effort: an unreadable ELF, a missing entry or any I/O
//! error is a miss and the caller spawns `size` as before. The board's limits
//! (`max_flash` / `max_ram`) are deliberately **not** part of the key: they come
//! from config and can change without a relink, and the caller re-applies them
//! after a restore. Entries are tiny JSON documents pruned oldest-first to
//! [`MAX_CACHE_BYTES`].

use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

use fbuild_core::SizeInfo;
use fbuild_core::path::NormalizedPath;

use crate::build_fingerprint::{load_json, save_json};

/// Upper bound on the cache's on-disk size. A report is ~200 bytes, so this
/// holds tens of thousands of distinct ELFs.
pub(crate) const MAX_CACHE_BYTES: u64 = 4 << 20;

/// Suffix of a cached report; also what pruning matches.
const SUFFIX: &str = ".json";

fn field(hasher: &mut sha2::Sha256, bytes: &[u8]) {
    use sha2::Digest;
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn entry_path(root: &Path, key: &str) -> NormalizedPath {
    NormalizedPath::new(root).join(format!("{key}{SUFFIX}"))
}

/// Content key for a size report.
///
/// Hashes the ELF's *contents* (never its path) plus `identity`: the values
/// besides the ELF that can change what the tool prints — here the version
/// stamp and the size binary itself. Returns `None` when the ELF cannot be
/// read, which makes the caller spawn the tool as it always did.
pub(crate) fn content_key(elf: &Path, identity: &[&str]) -> Option<String> {
    use sha2::{Digest, Sha256};

    let mut elf_digest = Sha256::new();
    let mut file = std::fs::File::open(elf).ok()?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        elf_digest.update(&buffer[..read]);
    }

    let mut hasher = Sha256::new();
    field(&mut hasher, &elf_digest.finalize());
    for value in identity {
        field(&mut hasher, value.as_bytes());
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// Read the report recorded for `key`, if any.
pub(crate) fn restore(root: &Path, key: &str) -> Option<SizeInfo> {
    load_json::<SizeInfo>(entry_path(root, key).as_path())
        .ok()
        .flatten()
}

/// Cache a freshly computed report. Best effort: a failure only costs the next
/// `size` spawn.
pub(crate) fn store(root: &Path, key: &str, size_info: &SizeInfo) {
    let entry = entry_path(root, key);
    if entry.as_path().is_file() {
        return;
    }
    if std::fs::create_dir_all(root).is_err() {
        return;
    }
    if save_json(entry.as_path(), size_info).is_err() {
        return;
    }
    prune_to(root, MAX_CACHE_BYTES);
}

/// Delete the oldest entries until the cache fits in `max_bytes`.
fn prune_to(root: &Path, max_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut total: u64 = 0;
    let mut reports: Vec<(SystemTime, u64, NormalizedPath)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || !name.ends_with(SUFFIX) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        total += metadata.len();
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        reports.push((modified, metadata.len(), NormalizedPath::new(entry.path())));
    }
    if total <= max_bytes {
        return;
    }
    reports.sort_by_key(|(modified, _, _)| *modified);
    for (_, len, path) in reports {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(path.as_path()).is_ok() {
            total -= len;
        }
    }
}

#[cfg(test)]
#[path = "size_report_cache_tests.rs"]
mod tests;
