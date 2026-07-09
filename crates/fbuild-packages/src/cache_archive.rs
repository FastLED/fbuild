//! First-class fbuild cache archiving: pack the fbuild cache (toolchains,
//! platforms, framework/package checkouts, downloaded archives, the sqlite
//! index, …) into a single self-describing `.tar.zst`, and restore it.
//!
//! This is the engine behind `fbuild cache save|restore|list|verify`
//! (FastLED/fbuild#527). It replaces the fragile "cache these six paths"
//! `actions/cache@v4` workaround every fbuild consumer grows in CI: one
//! archive, one key, and consumers stop having to know fbuild's internal cache
//! layout.
//!
//! Archive shape (mirrors soldr's `cache_lib` `.tar.zst`):
//!
//! ```text
//! FBUILD_CACHE_MANIFEST.pb   # prost manifest (version, slices, counts, hashes)
//! toolchains/...             # each included slice, prefixed by slice name
//! archives/...
//! installed/...
//! index                      # single-file slices store one bytes entry
//! ```
//!
//! The manifest is hand-written prost (no `protoc` / build.rs — the same
//! convention the rest of fbuild uses). v1 is a full snapshot; delta archives,
//! mtime replay, and a zccache `gha-cache` sidecar are explicit v2 items
//! (the `zccache` engine store is available here as an opt-in slice instead).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fbuild_core::{FbuildError, Result};
use prost::Message;
use sha2::{Digest, Sha256};

/// Manifest entry name inside the archive. Read first so `list`/`verify` never
/// have to stream the whole payload.
const MANIFEST_ENTRY: &str = "FBUILD_CACHE_MANIFEST.pb";

/// Bumped whenever the archive layout changes incompatibly.
const FORMAT_VERSION: u32 = 1;

/// Default zstd level. Matches setup-soldr's measured sweet spot: ~9 is a good
/// ratio/speed tradeoff for GHA cache payloads (19 is far slower for marginal
/// gains). Range 1..=22.
pub const DEFAULT_ZSTD_LEVEL: i32 = 9;

/// Which fbuild root a slice hangs off.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Root {
    /// `get_cache_root()` — `~/.fbuild/{dev|prod}/cache` (or `$FBUILD_CACHE_DIR`).
    Cache,
    /// `get_fbuild_root()` — `~/.fbuild/{dev|prod}` (for the `zccache` store).
    Fbuild,
}

/// A named, self-contained region of the cache.
struct SliceDef {
    /// Stable slice name (also the archive path prefix).
    name: &'static str,
    root: Root,
    /// Path of the slice relative to its root.
    rel: &'static str,
    /// A single file (e.g. `index.sqlite`) vs a directory tree.
    is_file: bool,
    /// Included in a default `save` (no `--include`).
    default: bool,
}

/// The full slice registry. Everything fbuild owns under the cache root is on
/// by default; the `zccache` engine store (a different root, content-addressed,
/// often better left to zccache itself) is opt-in.
const SLICES: &[SliceDef] = &[
    SliceDef {
        name: "toolchains",
        root: Root::Cache,
        rel: "toolchains",
        is_file: false,
        default: true,
    },
    SliceDef {
        name: "platforms",
        root: Root::Cache,
        rel: "platforms",
        is_file: false,
        default: true,
    },
    SliceDef {
        name: "packages",
        root: Root::Cache,
        rel: "packages",
        is_file: false,
        default: true,
    },
    SliceDef {
        name: "libraries",
        root: Root::Cache,
        rel: "libraries",
        is_file: false,
        default: true,
    },
    SliceDef {
        name: "archives",
        root: Root::Cache,
        rel: "archives",
        is_file: false,
        default: true,
    },
    SliceDef {
        name: "installed",
        root: Root::Cache,
        rel: "installed",
        is_file: false,
        default: true,
    },
    SliceDef {
        name: "index",
        root: Root::Cache,
        rel: "index.sqlite",
        is_file: true,
        default: true,
    },
    SliceDef {
        name: "zccache",
        root: Root::Fbuild,
        rel: "zccache",
        is_file: false,
        default: false,
    },
];

/// All slice names (for CLI help / validation).
pub fn all_slice_names() -> Vec<&'static str> {
    SLICES.iter().map(|s| s.name).collect()
}

/// Default slice names included by a bare `save`.
pub fn default_slice_names() -> Vec<&'static str> {
    SLICES
        .iter()
        .filter(|s| s.default)
        .map(|s| s.name)
        .collect()
}

fn find_slice(name: &str) -> Option<&'static SliceDef> {
    SLICES.iter().find(|s| s.name == name)
}

// ─── prost manifest (hand-written, no protoc) ────────────────────────────────

/// Per-slice inventory recorded in the archive manifest.
#[derive(Clone, PartialEq, Message)]
pub struct SliceInfo {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(uint64, tag = "2")]
    pub file_count: u64,
    #[prost(uint64, tag = "3")]
    pub byte_count: u64,
    /// Hex sha256 over the slice's sorted `(relpath, bytes)` stream.
    #[prost(string, tag = "4")]
    pub content_hash: String,
}

/// Top-level archive manifest.
#[derive(Clone, PartialEq, Message)]
pub struct CacheManifest {
    #[prost(uint32, tag = "1")]
    pub format_version: u32,
    #[prost(string, tag = "2")]
    pub fbuild_version: String,
    #[prost(message, repeated, tag = "3")]
    pub slices: Vec<SliceInfo>,
}

impl CacheManifest {
    /// Total bytes across all slices.
    pub fn total_bytes(&self) -> u64 {
        self.slices.iter().map(|s| s.byte_count).sum()
    }
    /// Total file count across all slices.
    pub fn total_files(&self) -> u64 {
        self.slices.iter().map(|s| s.file_count).sum()
    }
}

// ─── slice enumeration + hashing ─────────────────────────────────────────────

fn root_path(root: Root, cache_dir: &Path) -> PathBuf {
    match root {
        Root::Cache => cache_dir.to_path_buf(),
        // The zccache store is a sibling of `cache/` under the fbuild root.
        Root::Fbuild => cache_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(fbuild_paths::get_fbuild_root),
    }
}

/// Absolute source path of a slice on disk (may not exist).
fn slice_source(slice: &SliceDef, cache_dir: &Path) -> PathBuf {
    root_path(slice.root, cache_dir).join(slice.rel)
}

/// Sorted `(relpath, absolute_path)` list of every regular file under a slice.
/// `relpath` uses forward slashes and is relative to the slice source dir
/// (empty string for a single-file slice).
fn slice_files(slice: &SliceDef, cache_dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let src = slice_source(slice, cache_dir);
    if slice.is_file {
        return Ok(if src.is_file() {
            vec![(String::new(), src)]
        } else {
            Vec::new()
        });
    }
    if !src.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(&src).follow_links(false) {
        let entry =
            entry.map_err(|e| FbuildError::PackageError(format!("cache walk failed: {e}")))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&src)
            .map_err(|e| FbuildError::PackageError(format!("relpath: {e}")))?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        out.push((rel, entry.path().to_path_buf()));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// sha256 over the slice's sorted `(relpath\0 len bytes)` stream — a stable
/// content fingerprint independent of filesystem mtimes.
fn hash_slice(files: &[(String, Vec<u8>)]) -> String {
    let mut h = Sha256::new();
    for (rel, bytes) in files {
        h.update((rel.len() as u64).to_le_bytes());
        h.update(rel.as_bytes());
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    format!("{:x}", h.finalize())
}

/// Archive path for a slice file entry.
fn archive_path(slice_name: &str, rel: &str, is_file: bool) -> String {
    if is_file || rel.is_empty() {
        slice_name.to_string()
    } else {
        format!("{slice_name}/{rel}")
    }
}

// ─── save ────────────────────────────────────────────────────────────────────

/// Which slices to save.
pub enum SliceSelection {
    /// All `default` slices.
    Default,
    /// Exactly these slice names (validated).
    Explicit(Vec<String>),
}

fn resolve_selection(sel: &SliceSelection, exclude: &[String]) -> Result<Vec<&'static SliceDef>> {
    let mut names: Vec<&'static str> = match sel {
        SliceSelection::Default => default_slice_names(),
        SliceSelection::Explicit(v) => {
            let mut out = Vec::new();
            for n in v {
                let slice = find_slice(n).ok_or_else(|| {
                    FbuildError::PackageError(format!(
                        "unknown cache slice {n:?}; valid: {}",
                        all_slice_names().join(", ")
                    ))
                })?;
                out.push(slice.name);
            }
            out
        }
    };
    names.retain(|n| !exclude.iter().any(|e| e == n));
    Ok(names.into_iter().filter_map(find_slice).collect())
}

/// Write a `.tar.zst` snapshot of the selected cache slices. Returns the
/// manifest that was written (also embedded in the archive).
pub fn save(
    cache_dir: &Path,
    out_archive: &Path,
    selection: &SliceSelection,
    exclude: &[String],
    zstd_level: i32,
) -> Result<CacheManifest> {
    let slices = resolve_selection(selection, exclude)?;

    // Build the manifest + collect each slice's files (with contents, so the
    // hash is content- not mtime-based).
    let mut manifest = CacheManifest {
        format_version: FORMAT_VERSION,
        fbuild_version: env!("CARGO_PKG_VERSION").to_string(),
        slices: Vec::new(),
    };
    // (archive_path, bytes) pairs to write, in deterministic order.
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for slice in &slices {
        let files = slice_files(slice, cache_dir)?;
        let mut loaded: Vec<(String, Vec<u8>)> = Vec::with_capacity(files.len());
        let mut byte_count = 0u64;
        for (rel, abs) in &files {
            let bytes = std::fs::read(abs)
                .map_err(|e| FbuildError::PackageError(format!("read {}: {e}", abs.display())))?;
            byte_count += bytes.len() as u64;
            loaded.push((rel.clone(), bytes));
        }
        manifest.slices.push(SliceInfo {
            name: slice.name.to_string(),
            file_count: loaded.len() as u64,
            byte_count,
            content_hash: hash_slice(&loaded),
        });
        for (rel, bytes) in loaded {
            entries.push((archive_path(slice.name, &rel, slice.is_file), bytes));
        }
    }

    if let Some(parent) = out_archive.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let file = File::create(out_archive)
        .map_err(|e| FbuildError::PackageError(format!("create archive: {e}")))?;
    let level = zstd_level.clamp(1, 22);
    let encoder = zstd::stream::write::Encoder::new(file, level)
        .map_err(|e| FbuildError::PackageError(format!("zstd init: {e}")))?
        .auto_finish();
    let mut tar = tar::Builder::new(encoder);
    tar.mode(tar::HeaderMode::Deterministic);

    // Manifest first.
    let manifest_bytes = manifest.encode_to_vec();
    append_bytes(&mut tar, MANIFEST_ENTRY, &manifest_bytes)?;
    for (path, bytes) in &entries {
        append_bytes(&mut tar, path, bytes)?;
    }
    tar.finish()
        .map_err(|e| FbuildError::PackageError(format!("tar finish: {e}")))?;
    Ok(manifest)
}

fn append_bytes<W: Write>(tar: &mut tar::Builder<W>, path: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, path, bytes)
        .map_err(|e| FbuildError::PackageError(format!("tar append {path}: {e}")))
}

// ─── read manifest / list / verify ───────────────────────────────────────────

fn open_tar(
    archive: &Path,
) -> Result<tar::Archive<zstd::stream::read::Decoder<'static, std::io::BufReader<File>>>> {
    let file =
        File::open(archive).map_err(|e| FbuildError::PackageError(format!("open archive: {e}")))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|e| FbuildError::PackageError(format!("zstd open: {e}")))?;
    Ok(tar::Archive::new(decoder))
}

/// Read just the manifest (cheap — it's the first entry).
pub fn read_manifest(archive: &Path) -> Result<CacheManifest> {
    let mut tar = open_tar(archive)?;
    let entries = tar
        .entries()
        .map_err(|e| FbuildError::PackageError(format!("tar entries: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| FbuildError::PackageError(format!("tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| FbuildError::PackageError(format!("entry path: {e}")))?
            .to_string_lossy()
            .to_string();
        if path == MANIFEST_ENTRY {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| FbuildError::PackageError(format!("read manifest: {e}")))?;
            return CacheManifest::decode(buf.as_slice())
                .map_err(|e| FbuildError::PackageError(format!("decode manifest: {e}")));
        }
    }
    Err(FbuildError::PackageError(
        "archive has no FBUILD_CACHE_MANIFEST.pb — not an fbuild cache archive".into(),
    ))
}

/// Full integrity check: re-hash every slice's payload from the archive and
/// compare against the manifest's recorded `content_hash`. Returns the verified
/// manifest, or an error naming the first slice that fails.
pub fn verify(archive: &Path) -> Result<CacheManifest> {
    let manifest = read_manifest(archive)?;
    if manifest.format_version != FORMAT_VERSION {
        return Err(FbuildError::PackageError(format!(
            "archive format v{} not supported (this build understands v{FORMAT_VERSION})",
            manifest.format_version
        )));
    }
    // Rebuild per-slice (relpath, bytes) from the archive payload.
    let mut by_slice: BTreeMap<String, Vec<(String, Vec<u8>)>> = BTreeMap::new();
    let mut tar = open_tar(archive)?;
    for entry in tar
        .entries()
        .map_err(|e| FbuildError::PackageError(format!("tar entries: {e}")))?
    {
        let mut entry = entry.map_err(|e| FbuildError::PackageError(format!("tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| FbuildError::PackageError(format!("entry path: {e}")))?
            .to_string_lossy()
            .to_string();
        if path == MANIFEST_ENTRY {
            continue;
        }
        let (slice_name, rel) = split_archive_path(&path);
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| FbuildError::PackageError(format!("read {path}: {e}")))?;
        by_slice.entry(slice_name).or_default().push((rel, bytes));
    }
    for info in &manifest.slices {
        let mut files = by_slice.remove(&info.name).unwrap_or_default();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let got = hash_slice(&files);
        if got != info.content_hash {
            return Err(FbuildError::PackageError(format!(
                "slice {:?} failed verification: manifest {} != archive {}",
                info.name, info.content_hash, got
            )));
        }
    }
    Ok(manifest)
}

/// Split `toolchains/a/b.txt` → (`toolchains`, `a/b.txt`); `index` → (`index`, ``).
fn split_archive_path(path: &str) -> (String, String) {
    match path.split_once('/') {
        Some((slice, rel)) => (slice.to_string(), rel.to_string()),
        None => (path.to_string(), String::new()),
    }
}

// ─── restore ─────────────────────────────────────────────────────────────────

/// Extract every slice in the archive back into `cache_dir` (and the fbuild
/// root for the `zccache` slice). Returns the manifest.
pub fn restore(archive: &Path, cache_dir: &Path) -> Result<CacheManifest> {
    let manifest = read_manifest(archive)?;
    if manifest.format_version != FORMAT_VERSION {
        return Err(FbuildError::PackageError(format!(
            "archive format v{} not supported (this build understands v{FORMAT_VERSION})",
            manifest.format_version
        )));
    }
    std::fs::create_dir_all(cache_dir).ok();

    let mut tar = open_tar(archive)?;
    for entry in tar
        .entries()
        .map_err(|e| FbuildError::PackageError(format!("tar entries: {e}")))?
    {
        let mut entry = entry.map_err(|e| FbuildError::PackageError(format!("tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| FbuildError::PackageError(format!("entry path: {e}")))?
            .to_string_lossy()
            .to_string();
        if path == MANIFEST_ENTRY {
            continue;
        }
        // Reject path traversal.
        if path.contains("..") {
            return Err(FbuildError::PackageError(format!(
                "refusing unsafe archive path {path:?}"
            )));
        }
        let (slice_name, rel) = split_archive_path(&path);
        let Some(slice) = find_slice(&slice_name) else {
            // Unknown slice (forward-compat) — skip rather than fail.
            continue;
        };
        let dest = if slice.is_file {
            slice_source(slice, cache_dir)
        } else {
            slice_source(slice, cache_dir).join(&rel)
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                FbuildError::PackageError(format!("mkdir {}: {e}", parent.display()))
            })?;
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| FbuildError::PackageError(format!("read {path}: {e}")))?;
        std::fs::write(&dest, &bytes)
            .map_err(|e| FbuildError::PackageError(format!("write {}: {e}", dest.display())))?;
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, contents: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }

    fn seed_cache(cache: &Path) {
        write(cache, "toolchains/arm/bin/gcc.txt", "GCC");
        write(cache, "toolchains/arm/lib/libc.a", "LIBC");
        write(cache, "platforms/lpc8xx/core.h", "CORE");
        write(cache, "archives/arm-gcc.tar.gz", "TARBALL");
        write(cache, "installed/marker", "OK");
        write(cache, "index.sqlite", "SQLITE-DB");
    }

    #[test]
    fn round_trip_default_slices_reproduces_tree() {
        let src = tempfile::tempdir().unwrap();
        seed_cache(src.path());
        let archive = src.path().join("out.tar.zst");

        let saved = save(
            src.path(),
            &archive,
            &SliceSelection::Default,
            &[],
            DEFAULT_ZSTD_LEVEL,
        )
        .unwrap();
        assert!(
            saved.total_files() >= 6,
            "manifest files: {}",
            saved.total_files()
        );
        assert!(archive.is_file());

        // Restore into a fresh cache dir and compare byte-for-byte.
        let dst = tempfile::tempdir().unwrap();
        let restored = restore(&archive, dst.path()).unwrap();
        assert_eq!(restored.total_files(), saved.total_files());

        for rel in [
            "toolchains/arm/bin/gcc.txt",
            "toolchains/arm/lib/libc.a",
            "platforms/lpc8xx/core.h",
            "archives/arm-gcc.tar.gz",
            "installed/marker",
            "index.sqlite",
        ] {
            assert_eq!(
                std::fs::read(src.path().join(rel)).unwrap(),
                std::fs::read(dst.path().join(rel)).unwrap(),
                "mismatch restoring {rel}"
            );
        }
    }

    #[test]
    fn verify_passes_for_clean_archive_and_reads_manifest() {
        let src = tempfile::tempdir().unwrap();
        seed_cache(src.path());
        let archive = src.path().join("out.tar.zst");
        save(
            src.path(),
            &archive,
            &SliceSelection::Default,
            &[],
            DEFAULT_ZSTD_LEVEL,
        )
        .unwrap();

        let m = verify(&archive).unwrap();
        assert_eq!(m.format_version, FORMAT_VERSION);
        // list == read_manifest yields the same slices.
        let m2 = read_manifest(&archive).unwrap();
        assert_eq!(m, m2);
        assert!(m
            .slices
            .iter()
            .any(|s| s.name == "toolchains" && s.file_count == 2));
        assert!(m
            .slices
            .iter()
            .any(|s| s.name == "index" && s.file_count == 1));
    }

    #[test]
    fn include_exclude_selects_slices() {
        let src = tempfile::tempdir().unwrap();
        seed_cache(src.path());
        let archive = src.path().join("out.tar.zst");
        let saved = save(
            src.path(),
            &archive,
            &SliceSelection::Explicit(vec!["toolchains".into(), "archives".into()]),
            &["archives".into()],
            DEFAULT_ZSTD_LEVEL,
        )
        .unwrap();
        let names: Vec<_> = saved.slices.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["toolchains"],
            "only toolchains survives include−exclude"
        );
    }

    #[test]
    fn unknown_slice_name_is_an_error() {
        let src = tempfile::tempdir().unwrap();
        let archive = src.path().join("out.tar.zst");
        let err = save(
            src.path(),
            &archive,
            &SliceSelection::Explicit(vec!["not_a_slice".into()]),
            &[],
            DEFAULT_ZSTD_LEVEL,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("unknown cache slice"));
    }

    #[test]
    fn verify_detects_corruption() {
        let src = tempfile::tempdir().unwrap();
        seed_cache(src.path());
        let archive = src.path().join("out.tar.zst");
        let m = save(src.path(), &archive, &SliceSelection::Default, &[], 3).unwrap();

        // Tamper: re-save with a mutated file but splice the OLD manifest back
        // in is complex; instead assert verify() passes here and corruption is
        // caught by the hash mismatch path via a hand-built bad manifest.
        let bad = CacheManifest {
            slices: m
                .slices
                .iter()
                .map(|s| SliceInfo {
                    content_hash: "deadbeef".into(),
                    ..s.clone()
                })
                .collect(),
            ..m.clone()
        };
        // A manifest whose hashes don't match the payload must fail verify.
        // Re-pack with the bad manifest by writing a fresh archive.
        let archive2 = src.path().join("bad.tar.zst");
        write_archive_with_manifest(src.path(), &archive2, &bad);
        let err = verify(&archive2).unwrap_err();
        assert!(
            format!("{err}").contains("failed verification"),
            "got: {err}"
        );
    }

    /// Test helper: pack the default slices but embed a caller-supplied
    /// (possibly wrong) manifest, to exercise the verify() mismatch path.
    fn write_archive_with_manifest(cache_dir: &Path, out: &Path, manifest: &CacheManifest) {
        let file = File::create(out).unwrap();
        let encoder = zstd::stream::write::Encoder::new(file, 3)
            .unwrap()
            .auto_finish();
        let mut tar = tar::Builder::new(encoder);
        tar.mode(tar::HeaderMode::Deterministic);
        append_bytes(&mut tar, MANIFEST_ENTRY, &manifest.encode_to_vec()).unwrap();
        for slice in SLICES.iter().filter(|s| s.default) {
            for (rel, abs) in slice_files(slice, cache_dir).unwrap() {
                let bytes = std::fs::read(&abs).unwrap();
                append_bytes(
                    &mut tar,
                    &archive_path(slice.name, &rel, slice.is_file),
                    &bytes,
                )
                .unwrap();
            }
        }
        tar.finish().unwrap();
    }
}
