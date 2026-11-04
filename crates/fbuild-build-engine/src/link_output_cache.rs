//! Cross-project reuse of link outputs (FastLED/fbuild#1894).
//!
//! Fresh project directories recompile to byte-identical objects (the
//! prefix-map work in #1893 normalizes every path channel), yet every fresh
//! directory still pays a full link. `link_digest` cannot help: it hashes the
//! absolute *paths* of the link inputs, which differ per project.
//!
//! This cache keys the link output on the *contents* of the link inputs plus
//! the link flags with the output directory normalized away. A hit restores
//! `firmware.elf` and lets the normal convert/size legs run from it, so the
//! ELF's debug info stays identical (verified byte-for-byte across two fresh
//! dirs) without paying the linker.
//!
//! The cache is strictly best effort: any I/O error, missing input or
//! unreadable entry is a miss and the caller relinks. Entries are pruned to
//! [`MAX_CACHE_BYTES`] by directory mtime so the cache cannot grow without
//! bound.

use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

use fbuild_core::path::NormalizedPath;

use crate::linker::LinkExtraArgs;

/// Upper bound on the cache's on-disk size. A blink ELF is ~6 MB, so this
/// holds on the order of a hundred distinct links before the oldest go.
pub(crate) const MAX_CACHE_BYTES: u64 = 1 << 30;

/// File name of the cached ELF inside an entry directory.
const ELF_NAME: &str = "firmware.elf";

fn field(hasher: &mut sha2::Sha256, bytes: &[u8]) {
    use sha2::Digest;
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Digest of the link inputs' *contents*, independent of the directory they
/// happen to live in.
///
/// Returns `None` when any input cannot be read: the caller then simply
/// relinks, which is always correct.
pub(crate) fn content_key(
    inputs: &[&Path],
    extra: &LinkExtraArgs,
    platform_fingerprint: &str,
    output_dir: &Path,
) -> Option<String> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    field(&mut hasher, env!("CARGO_PKG_VERSION").as_bytes());
    field(&mut hasher, platform_fingerprint.as_bytes());
    field(&mut hasher, &[u8::from(extra.bloat_analysis)]);
    let out = output_dir.to_string_lossy();
    for flag in &extra.flags {
        field(&mut hasher, flag.replace(out.as_ref(), "<out>").as_bytes());
    }
    field(&mut hasher, b"--libs");
    for lib in &extra.libs {
        field(&mut hasher, lib.replace(out.as_ref(), "<out>").as_bytes());
    }
    field(&mut hasher, b"--inputs");
    for input in inputs {
        let mut file = std::fs::File::open(input).ok()?;
        let mut buffer = [0u8; 64 * 1024];
        let mut contents = Sha256::new();
        loop {
            let read = file.read(&mut buffer).ok()?;
            if read == 0 {
                break;
            }
            contents.update(&buffer[..read]);
        }
        field(&mut hasher, &contents.finalize());
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// Directory holding the entry for `key`.
fn entry_dir(root: &Path, key: &str) -> NormalizedPath {
    let prefix = key.get(..2).unwrap_or("00");
    NormalizedPath::new(root.join(prefix).join(key))
}

/// Copy a cached ELF for `key` into `elf_out`. `false` means "no usable
/// entry" and the caller must relink.
pub(crate) fn restore(root: &Path, key: &str, elf_out: &Path) -> bool {
    let cached = entry_dir(root, key).join(ELF_NAME);
    if !cached.is_file() {
        return false;
    }
    if let Some(parent) = elf_out.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let staged = elf_out.with_extension("elf.link-restore");
    if std::fs::copy(&cached, &staged).is_err() {
        return false;
    }
    std::fs::rename(&staged, elf_out).is_ok()
}

/// Record `elf` as the output for `key`, then prune the cache if needed.
pub(crate) fn store(root: &Path, key: &str, elf: &Path) {
    let dir = entry_dir(root, key);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let target = dir.join(ELF_NAME);
    let staged = dir.join("firmware.elf.tmp");
    if std::fs::copy(elf, &staged).is_err() || std::fs::rename(&staged, &target).is_err() {
        let _ = std::fs::remove_file(&staged);
        return;
    }
    prune(root);
}

/// Total size and modification time of an entry directory.
fn entry_size(dir: &Path) -> (u64, Option<SystemTime>) {
    let mut size = 0u64;
    let mut mtime = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                size = size.saturating_add(metadata.len());
                if metadata.is_dir() {
                    let (nested, nested_mtime) = entry_size(&entry.path());
                    size = size.saturating_add(nested);
                    mtime = mtime.max(nested_mtime);
                } else if let Ok(modified) = metadata.modified() {
                    mtime = mtime.max(Some(modified));
                }
            }
        }
    }
    (size, mtime)
}

/// Drop the oldest entries until the cache fits [`MAX_CACHE_BYTES`].
fn prune(root: &Path) {
    let Ok(prefixes) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<(SystemTime, u64, NormalizedPath)> = Vec::new();
    let mut total = 0u64;
    for prefix in prefixes.flatten() {
        let prefix_path = prefix.path();
        if !prefix_path.is_dir() {
            continue;
        }
        let Ok(dir_entries) = std::fs::read_dir(&prefix_path) else {
            continue;
        };
        for entry in dir_entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let (size, mtime) = entry_size(&path);
            total = total.saturating_add(size);
            entries.push((
                mtime.unwrap_or(SystemTime::UNIX_EPOCH),
                size,
                NormalizedPath::new(&path),
            ));
        }
    }
    if total <= MAX_CACHE_BYTES {
        return;
    }
    entries.sort_by_key(|(mtime, _, _)| *mtime);
    for (_, size, path) in entries {
        if total <= MAX_CACHE_BYTES {
            break;
        }
        if std::fs::remove_dir_all(path.as_path()).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_object(dir: &Path, name: &str, bytes: &[u8]) -> NormalizedPath {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        NormalizedPath::new(&path)
    }

    fn key(dir: &Path, object: &Path, flags: &[String]) -> String {
        let extra = LinkExtraArgs {
            flags: flags.to_vec(),
            ..Default::default()
        };
        content_key(&[object], &extra, "fingerprint", dir).unwrap()
    }

    #[test]
    fn key_ignores_the_directory_that_holds_the_inputs() {
        let one = tempfile::TempDir::new().unwrap();
        let two = tempfile::TempDir::new().unwrap();
        let a = write_object(one.path(), "sketch.o", b"same bytes");
        let b = write_object(two.path(), "sketch.o", b"same bytes");
        let flag_a = format!("-Wl,-Map={}/firmware.map", one.path().display());
        let flag_b = format!("-Wl,-Map={}/firmware.map", two.path().display());
        assert_eq!(
            key(one.path(), &a, &[flag_a]),
            key(two.path(), &b, &[flag_b])
        );
    }

    #[test]
    fn key_changes_with_object_contents_flags_and_fingerprint() {
        let dir = tempfile::TempDir::new().unwrap();
        let object = write_object(dir.path(), "sketch.o", b"one");
        let base = key(dir.path(), &object, &["-Wl,--gc-sections".to_string()]);
        std::fs::write(&object, b"two").unwrap();
        assert_ne!(
            base,
            key(dir.path(), &object, &["-Wl,--gc-sections".to_string()])
        );
        assert_ne!(base, key(dir.path(), &object, &[]));
        let extra = LinkExtraArgs::default();
        assert_ne!(
            content_key(&[&object], &extra, "one", dir.path()),
            content_key(&[&object], &extra, "two", dir.path())
        );
    }

    #[test]
    fn a_missing_input_is_a_miss_instead_of_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("gone.o");
        let extra = LinkExtraArgs::default();
        assert!(content_key(&[&missing], &extra, "", dir.path()).is_none());
    }

    #[test]
    fn store_then_restore_round_trips_the_elf() {
        let cache = tempfile::TempDir::new().unwrap();
        let work = tempfile::TempDir::new().unwrap();
        let built = write_object(work.path(), "built.elf", b"\x7fELF output");
        store(cache.path(), "abc123", &built);

        let fresh = tempfile::TempDir::new().unwrap();
        let restored = fresh.path().join("firmware.elf");
        assert!(restore(cache.path(), "abc123", &restored));
        assert_eq!(std::fs::read(&restored).unwrap(), b"\x7fELF output");
        assert!(!restore(cache.path(), "missing", &restored));
    }
}
