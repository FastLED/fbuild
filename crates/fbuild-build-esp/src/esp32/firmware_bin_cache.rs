//! Cross-project reuse of the `elf2image` product (FastLED/fbuild#1894).
//!
//! [`crate::linker`]'s `link_output_cache` removes the link itself for a fresh
//! project directory whose objects are byte-identical to a previous one. What
//! remains on that path is `esptool elf2image`: it measured **391 ms of a
//! 1332 ms** fresh-directory build (probe `p1894b`, 2026-11-04) once the ELF
//! came from the cache — the single largest phase left, ~29% of the build.
//!
//! The converted image is a pure function of the linked ELF plus the flash
//! parameters handed to esptool, and the ELF is already byte-identical across
//! fresh directories (verified: `md5sum` equal for two fresh dirs, and equal
//! `firmware.bin` too). So a directory that restores an ELF it has never built
//! can also restore the image that ELF converts to.
//!
//! Reuse is strictly best effort: an unreadable ELF, a missing entry or any
//! I/O error is a miss and the caller runs esptool as before. Entries are
//! pruned oldest-first to [`MAX_CACHE_BYTES`] so the cache cannot grow without
//! bound.

use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

use fbuild_core::path::NormalizedPath;

/// Upper bound on the cache's on-disk size. A blink image is ~310 KB, so this
/// holds well over five hundred distinct images.
pub(crate) const MAX_CACHE_BYTES: u64 = 256 << 20;

/// Suffix of a cached image; also what pruning matches.
const SUFFIX: &str = ".bin";

fn field(hasher: &mut sha2::Sha256, bytes: &[u8]) {
    use sha2::Digest;
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn entry_path(root: &Path, key: &str) -> NormalizedPath {
    NormalizedPath::new(root).join(format!("{key}{SUFFIX}"))
}

fn temp_path(root: &Path, key: &str) -> NormalizedPath {
    NormalizedPath::new(root).join(format!(".{key}.{}.tmp", std::process::id()))
}

/// Content key for a converted image.
///
/// Hashes the ELF's *contents* (never its path) plus `identity`: every value
/// besides the ELF that changes the bytes esptool emits — chip, flash mode,
/// frequency, size, and the esptool binary that performs the conversion.
///
/// Returns `None` when the ELF cannot be read; the caller then converts as it
/// always did, which is always correct.
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

/// Copy a cached image to `dest`.
///
/// Returns `false` on any miss or I/O error, leaving `dest` untouched: the
/// caller converts instead. The copy lands through a temporary sibling so a
/// reader never sees a half-written image.
pub(crate) fn restore(root: &Path, key: &str, dest: &Path) -> bool {
    let entry = entry_path(root, key);
    if !entry.as_path().is_file() {
        return false;
    }
    let temp = temp_path(root, key);
    if std::fs::copy(entry.as_path(), temp.as_path()).is_err() {
        let _ = std::fs::remove_file(temp.as_path());
        return false;
    }
    if std::fs::rename(temp.as_path(), dest).is_ok() {
        return true;
    }
    // The cache root and the project directory can live on different
    // filesystems, where rename is `EXDEV`. Fall back to a direct copy.
    let copied = std::fs::copy(entry.as_path(), dest).is_ok();
    let _ = std::fs::remove_file(temp.as_path());
    copied
}

/// Cache a freshly converted image. Best effort: failures only cost the next
/// conversion.
pub(crate) fn store(root: &Path, key: &str, source: &Path) {
    let entry = entry_path(root, key);
    if entry.as_path().is_file() {
        return;
    }
    if std::fs::create_dir_all(root).is_err() {
        return;
    }
    let temp = temp_path(root, key);
    if std::fs::copy(source, temp.as_path()).is_err() {
        let _ = std::fs::remove_file(temp.as_path());
        return;
    }
    if std::fs::rename(temp.as_path(), entry.as_path()).is_err() {
        let _ = std::fs::remove_file(temp.as_path());
        return;
    }
    prune_to(root, MAX_CACHE_BYTES);
}

/// Delete the oldest entries until the cache fits in `max_bytes`.
///
/// Ordered by mtime, so the image a fresh directory builds next — the one most
/// likely to be the newest link — survives.
pub(crate) fn prune_to(root: &Path, max_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut total: u64 = 0;
    let mut images: Vec<(SystemTime, u64, NormalizedPath)> = Vec::new();
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
        images.push((modified, metadata.len(), NormalizedPath::new(entry.path())));
    }
    if total <= max_bytes {
        return;
    }
    images.sort_by_key(|(modified, _, _)| *modified);
    for (_, len, path) in images {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(path.as_path()).is_ok() {
            total -= len;
        }
    }
}
