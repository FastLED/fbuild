//! Persisted metadata for top-level no-op build fast paths.

pub mod fast_path;

pub use fast_path::{fast_path_check, fast_path_watch, FastPathHit, FastPathInputs};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use fbuild_core::{Result, SizeInfo};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::zccache::FingerprintWatch;

pub const BUILD_FINGERPRINT_VERSION: u32 = 2;
const WATCH_STAMP_CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedBuildFingerprint {
    pub version: u32,
    pub metadata_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_set_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_info: Option<SizeInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileStamp {
    pub len: u64,
    pub modified_secs: u64,
    pub modified_nanos: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WatchFileStamp {
    pub relative_path: String,
    pub stamp: FileStamp,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PersistedWatchStampCache {
    pub version: u32,
    pub files: Vec<WatchFileStamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BinArtifactCache {
    pub version: u32,
    pub elf_stamp: FileStamp,
    pub flash_mode: String,
    pub flash_freq: String,
    pub flash_size: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizeArtifactCache {
    pub version: u32,
    pub elf_stamp: FileStamp,
    pub size_info: SizeInfo,
}

impl Default for PersistedBuildFingerprint {
    fn default() -> Self {
        Self {
            version: BUILD_FINGERPRINT_VERSION,
            metadata_hash: String::new(),
            file_set_hash: None,
            size_info: None,
        }
    }
}

impl FileStamp {
    pub fn from_path(path: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let modified = metadata.modified()?;
        let duration = modified.duration_since(UNIX_EPOCH).map_err(|e| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to convert mtime for {}: {}",
                path.display(),
                e
            ))
        })?;
        Ok(Self {
            len: metadata.len(),
            modified_secs: duration.as_secs(),
            modified_nanos: duration.subsec_nanos(),
        })
    }
}

pub fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub fn normalize_paths(paths: &[PathBuf]) -> Vec<String> {
    let mut normalized: Vec<String> = paths.iter().map(|p| normalize_path(p)).collect();
    normalized.sort();
    normalized
}

pub fn stable_hash_json<T: Serialize>(value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!("failed to serialize fingerprint input: {e}"))
    })?;
    Ok(sha256_hex(&bytes))
}

pub fn hash_files(paths: &[PathBuf]) -> Result<String> {
    let mut sorted = paths.to_vec();
    sorted.sort();
    let mut hasher = Sha256::new();
    for path in &sorted {
        if !path.exists() {
            hasher.update(b"missing\0");
            hasher.update(normalize_path(path));
            hasher.update(b"\0");
            continue;
        }
        hasher.update(b"path\0");
        hasher.update(normalize_path(path));
        hasher.update(b"\0");
        let bytes = std::fs::read(path)?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn hash_watch_set(watches: &[FingerprintWatch]) -> Result<String> {
    let mut ordered = watches.to_vec();
    ordered.sort_by_key(watch_identity);

    let mut hasher = Sha256::new();
    for watch in &ordered {
        hasher.update(b"watch\0");
        hasher.update(watch_identity(watch));
        hasher.update(b"\0");
        if !watch.root.exists() {
            hasher.update(b"missing-root\0");
            continue;
        }

        let mut files = Vec::new();
        for entry in WalkDir::new(&watch.root)
            .into_iter()
            .filter_entry(|entry| should_descend(entry.path(), &watch.root, &watch.excludes))
            .flatten()
        {
            if !entry.file_type().is_file() {
                continue;
            }
            if !matches_extension(entry.path(), &watch.extensions) {
                continue;
            }
            files.push(entry.into_path());
        }
        files.sort();

        for file in files {
            hasher.update(b"file\0");
            hasher.update(relative_path_for_hash(&watch.root, &file));
            hasher.update(b"\0");
            let bytes = std::fs::read(&file)?;
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(blake3::hash(&bytes).as_bytes());
        }
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// In-memory cache for [`hash_watch_set_stamps`] results across
/// invocations within the same daemon lifetime. The daemon implements
/// this so warm rebuilds within a few seconds of each other can skip
/// the per-build walk over thousands of watched files.
///
/// The cache key is derived by the implementor from the watch set's
/// root paths; the orchestrator just hands the slice in. Callers must
/// expect `get` to return `None` whenever the implementation considers
/// the entry stale (typically a 2–5 s freshness window since the last
/// `put`), so correctness is unaffected by an absent or evicted entry.
pub trait WatchSetStampCache: Send + Sync {
    fn get(&self, watches: &[FingerprintWatch]) -> Option<String>;
    fn put(&self, watches: &[FingerprintWatch], hash: String);
}

/// [`hash_watch_set_stamps`] with an optional in-memory short-circuit.
///
/// When `cache` is `Some`, a cache hit returns immediately without
/// walking the watch tree (the dominant cost for large projects per
/// `docs/PERF_WARM_BUILD.md`). On miss, the result is recorded
/// before being returned.
///
/// `cache: None` is identical to calling [`hash_watch_set_stamps`]
/// directly — used by code paths (CLI, tests) that don't have a
/// daemon-scoped cache to consult.
pub fn hash_watch_set_stamps_cached(
    watches: &[FingerprintWatch],
    cache: Option<&dyn WatchSetStampCache>,
) -> Result<String> {
    if let Some(c) = cache {
        if let Some(hash) = c.get(watches) {
            return Ok(hash);
        }
    }
    let hash = hash_watch_set_stamps(watches)?;
    if let Some(c) = cache {
        c.put(watches, hash.clone());
    }
    Ok(hash)
}

pub fn hash_watch_set_stamps(watches: &[FingerprintWatch]) -> Result<String> {
    hash_watch_set_stamps_inner(watches, content_hash_hex)
}

fn hash_watch_set_stamps_inner<F>(
    watches: &[FingerprintWatch],
    mut content_hash: F,
) -> Result<String>
where
    F: FnMut(&Path) -> Result<String>,
{
    let mut ordered = watches.to_vec();
    ordered.sort_by_key(watch_identity);

    let mut hasher = Sha256::new();
    for watch in &ordered {
        hasher.update(b"watch\0");
        hasher.update(watch_identity(watch));
        hasher.update(b"\0");
        if !watch.root.exists() {
            hasher.update(b"missing-root\0");
            continue;
        }

        let previous_cache = load_watch_stamp_cache(&watch_stamp_cache_path(watch));
        let previous_files: HashMap<String, WatchFileStamp> = previous_cache
            .files
            .into_iter()
            .map(|file| (file.relative_path.clone(), file))
            .collect();
        let mut next_files = Vec::new();

        let mut files = Vec::new();
        for entry in WalkDir::new(&watch.root)
            .into_iter()
            .filter_entry(|entry| should_descend(entry.path(), &watch.root, &watch.excludes))
            .flatten()
        {
            if !entry.file_type().is_file() {
                continue;
            }
            if !matches_extension(entry.path(), &watch.extensions) {
                continue;
            }
            files.push(entry.into_path());
        }
        files.sort();

        for file in files {
            let relative_path = relative_path_for_hash(&watch.root, &file);
            let stamp = FileStamp::from_path(&file)?;
            let content_hash = match previous_files.get(&relative_path) {
                Some(previous) if previous.stamp == stamp => previous.content_hash.clone(),
                _ => content_hash(&file)?,
            };

            hasher.update(b"file\0");
            hasher.update(&relative_path);
            hasher.update(b"\0");
            hasher.update(stamp.len.to_le_bytes());
            hasher.update(content_hash.as_bytes());

            next_files.push(WatchFileStamp {
                relative_path,
                stamp,
                content_hash,
            });
        }

        save_json(
            &watch_stamp_cache_path(watch),
            &PersistedWatchStampCache {
                version: WATCH_STAMP_CACHE_VERSION,
                files: next_files,
            },
        )?;
    }

    Ok(format!("{:x}", hasher.finalize()))
}

pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    let value = serde_json::from_slice(&bytes).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!("failed to parse {}: {}", path.display(), e))
    })?;
    Ok(Some(value))
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to serialize {}: {}",
            path.display(),
            e
        ))
    })?;
    write_if_changed(path, &bytes)
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Ok(existing) = std::fs::read(path) {
        if existing == bytes {
            return Ok(());
        }
    }
    std::fs::write(path, bytes)?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn content_hash_hex(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn relative_path_for_hash(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    normalize_path(relative)
}

fn watch_identity(watch: &FingerprintWatch) -> String {
    watch
        .cache_file
        .file_name()
        .map(|name| name.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| normalize_path(&watch.root))
}

fn watch_stamp_cache_path(watch: &FingerprintWatch) -> PathBuf {
    watch.cache_file.with_extension("stamp_cache.json")
}

fn load_watch_stamp_cache(path: &Path) -> PersistedWatchStampCache {
    match load_json::<PersistedWatchStampCache>(path) {
        Ok(Some(cache)) if cache.version == WATCH_STAMP_CACHE_VERSION => cache,
        _ => PersistedWatchStampCache {
            version: WATCH_STAMP_CACHE_VERSION,
            files: Vec::new(),
        },
    }
}

fn matches_extension(path: &Path, extensions: &[String]) -> bool {
    if extensions.is_empty() {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    extensions
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&ext))
}

fn should_descend(path: &Path, root: &Path, excludes: &[String]) -> bool {
    if path == root {
        return true;
    }
    if !path.is_dir() {
        return true;
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    !excludes.iter().any(|exclude| exclude == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;

    fn make_watch(root: &Path, cache_file: &Path) -> FingerprintWatch {
        FingerprintWatch {
            cache_file: cache_file.to_path_buf(),
            root: root.to_path_buf(),
            extensions: vec!["cpp".to_string(), "h".to_string()],
            excludes: vec!["build".to_string()],
        }
    }

    fn hash_watch_with_counter(watch: &FingerprintWatch, hash_count: &Cell<usize>) -> String {
        hash_watch_set_stamps_inner(std::slice::from_ref(watch), |path| {
            hash_count.set(hash_count.get() + 1);
            content_hash_hex(path)
        })
        .unwrap()
    }

    #[test]
    fn test_hash_files_changes_when_contents_change() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.txt");
        fs::write(&path, "one").unwrap();
        let first = hash_files(std::slice::from_ref(&path)).unwrap();
        fs::write(&path, "two").unwrap();
        let second = hash_files(std::slice::from_ref(&path)).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn test_hash_watch_set_changes_when_watched_source_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let main = src.join("main.cpp");
        fs::write(&main, "int main() { return 1; }\n").unwrap();

        let watch = make_watch(&src, &tmp.path().join("watch.json"));

        let first = hash_watch_set(std::slice::from_ref(&watch)).unwrap();
        fs::write(&main, "int main() { return 2; }\n").unwrap();
        let second = hash_watch_set(std::slice::from_ref(&watch)).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn test_save_json_does_not_rewrite_unchanged_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("fingerprint.json");
        let value = PersistedBuildFingerprint {
            version: BUILD_FINGERPRINT_VERSION,
            metadata_hash: "abc".to_string(),
            file_set_hash: Some("def".to_string()),
            size_info: None,
        };

        save_json(&path, &value).unwrap();
        let first_mtime = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        save_json(&path, &value).unwrap();
        let second_mtime = fs::metadata(&path).unwrap().modified().unwrap();

        assert_eq!(first_mtime, second_mtime);
    }

    #[test]
    fn test_hash_watch_set_stamps_changes_when_contents_change() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let main = src.join("main.cpp");
        fs::write(&main, "int main() { return 1; }\n").unwrap();

        let watch = make_watch(&src, &tmp.path().join("watch.json"));

        let first = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&main, "int main() { return 1; }\n// touch\n").unwrap();
        let second = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn test_hash_watch_set_stamps_ignores_mtime_only_change() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let main = src.join("main.cpp");
        let contents = "int main() { return 1; }\n";
        fs::write(&main, contents).unwrap();

        let watch = make_watch(&src, &tmp.path().join(".project.zccache_fp.json"));

        let first = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&main, contents).unwrap();
        let second = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn test_hash_watch_set_stamps_skips_rehash_when_stamp_unchanged() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let main = src.join("main.cpp");
        fs::write(&main, "int main() { return 1; }\n").unwrap();

        let watch = make_watch(&src, &tmp.path().join(".project.zccache_fp.json"));

        let hash_count = Cell::new(0usize);
        let first = hash_watch_with_counter(&watch, &hash_count);
        let after_first = hash_count.get();
        let second = hash_watch_with_counter(&watch, &hash_count);

        assert_eq!(first, second);
        assert_eq!(
            after_first, 1,
            "initial pass should hash file contents once"
        );
        assert_eq!(
            hash_count.get(),
            after_first,
            "unchanged mtime/size should reuse cached content hash"
        );
    }

    #[test]
    fn test_hash_watch_set_stamps_rehashes_only_changed_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let main = src.join("main.cpp");
        let header = src.join("pins.h");
        fs::write(&main, "int main() { return LED_PIN; }\n").unwrap();
        fs::write(&header, "#define LED_PIN 1\n").unwrap();

        let watch = make_watch(&src, &tmp.path().join(".project.zccache_fp.json"));
        let hash_count = Cell::new(0usize);

        let first = hash_watch_with_counter(&watch, &hash_count);
        let after_first = hash_count.get();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&header, "#define LED_PIN 2\n").unwrap();
        let second = hash_watch_with_counter(&watch, &hash_count);

        assert_ne!(first, second);
        assert_eq!(after_first, 2, "first pass should hash both tracked files");
        assert_eq!(
            hash_count.get(),
            after_first + 1,
            "only the file whose stamp changed should be rehashed"
        );
    }

    #[test]
    fn test_hash_watch_set_stamps_rehashes_once_for_mtime_change_then_updates_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let main = src.join("main.cpp");
        let contents = "int main() { return 1; }\n";
        fs::write(&main, contents).unwrap();

        let watch = make_watch(&src, &tmp.path().join(".project.zccache_fp.json"));
        let hash_count = Cell::new(0usize);

        let first = hash_watch_with_counter(&watch, &hash_count);
        let after_first = hash_count.get();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&main, contents).unwrap();
        let second = hash_watch_with_counter(&watch, &hash_count);
        let after_second = hash_count.get();
        let third = hash_watch_with_counter(&watch, &hash_count);

        assert_eq!(first, second);
        assert_eq!(second, third);
        assert_eq!(after_first, 1, "initial pass should hash contents once");
        assert_eq!(
            after_second,
            after_first + 1,
            "mtime-only change should trigger exactly one rehash"
        );
        assert_eq!(
            hash_count.get(),
            after_second,
            "subsequent unchanged run should reuse the updated stamp cache"
        );
    }

    #[test]
    fn test_hash_watch_set_stamps_adds_new_file_without_rehashing_unchanged_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let main = src.join("main.cpp");
        fs::write(&main, "int main() { return 0; }\n").unwrap();

        let watch = make_watch(&src, &tmp.path().join(".project.zccache_fp.json"));
        let hash_count = Cell::new(0usize);

        let first = hash_watch_with_counter(&watch, &hash_count);
        let after_first = hash_count.get();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(src.join("config.h"), "#define VALUE 42\n").unwrap();
        let second = hash_watch_with_counter(&watch, &hash_count);

        assert_ne!(first, second);
        assert_eq!(after_first, 1, "initial pass should hash the existing file");
        assert_eq!(
            hash_count.get(),
            after_first + 1,
            "adding a file should hash only the new entry"
        );
    }

    #[test]
    fn test_hash_watch_set_stamps_removes_deleted_file_without_rehashing_unchanged_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let main = src.join("main.cpp");
        let header = src.join("pins.h");
        fs::write(&main, "int main() { return LED_PIN; }\n").unwrap();
        fs::write(&header, "#define LED_PIN 1\n").unwrap();

        let watch = make_watch(&src, &tmp.path().join(".project.zccache_fp.json"));
        let hash_count = Cell::new(0usize);

        let first = hash_watch_with_counter(&watch, &hash_count);
        let after_first = hash_count.get();
        fs::remove_file(&header).unwrap();
        let second = hash_watch_with_counter(&watch, &hash_count);

        assert_ne!(first, second);
        assert_eq!(after_first, 2, "initial pass should hash both files");
        assert_eq!(
            hash_count.get(),
            after_first,
            "deleting a file should not force rehashing unchanged survivors"
        );
    }

    #[test]
    fn test_hash_watch_set_stamps_ignores_corrupt_stamp_cache_and_rebuilds() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let main = src.join("main.cpp");
        fs::write(&main, "int main() { return 0; }\n").unwrap();

        let watch = make_watch(&src, &tmp.path().join(".project.zccache_fp.json"));
        let first = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();
        fs::write(watch_stamp_cache_path(&watch), "{ not valid json").unwrap();

        let hash_count = Cell::new(0usize);
        let second = hash_watch_with_counter(&watch, &hash_count);

        assert_eq!(first, second);
        assert_eq!(
            hash_count.get(),
            1,
            "corrupt sidecar cache should be ignored and rebuilt from content"
        );
    }

    #[test]
    fn test_hash_watch_set_stamps_ignores_unknown_stamp_cache_version() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let main = src.join("main.cpp");
        fs::write(&main, "int main() { return 0; }\n").unwrap();

        let watch = make_watch(&src, &tmp.path().join(".project.zccache_fp.json"));
        let first = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();
        save_json(
            &watch_stamp_cache_path(&watch),
            &PersistedWatchStampCache {
                version: WATCH_STAMP_CACHE_VERSION + 1,
                files: Vec::new(),
            },
        )
        .unwrap();

        let hash_count = Cell::new(0usize);
        let second = hash_watch_with_counter(&watch, &hash_count);

        assert_eq!(first, second);
        assert_eq!(
            hash_count.get(),
            1,
            "unknown sidecar cache version should be treated as cold and rebuilt"
        );
    }

    #[test]
    fn test_hash_watch_set_stamps_excludes_filtered_directories_adversarially() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let build_dir = src.join("build");
        fs::create_dir_all(&build_dir).unwrap();
        fs::write(src.join("main.cpp"), "int main() { return 0; }\n").unwrap();
        fs::write(build_dir.join("generated.h"), "#define GENERATED 1\n").unwrap();

        let watch = make_watch(&src, &tmp.path().join(".project.zccache_fp.json"));
        let first = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(build_dir.join("generated.h"), "#define GENERATED 2\n").unwrap();
        let second = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();

        assert_eq!(
            first, second,
            "changes under excluded directories must not poison the watch fingerprint"
        );
    }

    #[test]
    fn test_hash_watch_set_ignores_workspace_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a_src = tmp.path().join("workspace-a").join("project");
        let b_src = tmp.path().join("workspace-b").join("project");
        fs::create_dir_all(&a_src).unwrap();
        fs::create_dir_all(&b_src).unwrap();
        fs::write(a_src.join("main.cpp"), "int main() { return 0; }\n").unwrap();
        fs::write(b_src.join("main.cpp"), "int main() { return 0; }\n").unwrap();

        let a_watch = make_watch(
            &a_src,
            &tmp.path()
                .join("workspace-a")
                .join(".project.zccache_fp.json"),
        );
        let b_watch = make_watch(
            &b_src,
            &tmp.path()
                .join("workspace-b")
                .join(".project.zccache_fp.json"),
        );

        let a_hash = hash_watch_set(std::slice::from_ref(&a_watch)).unwrap();
        let b_hash = hash_watch_set(std::slice::from_ref(&b_watch)).unwrap();

        assert_eq!(a_hash, b_hash);
    }

    #[test]
    fn test_hash_watch_set_stamps_ignores_workspace_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a_src = tmp.path().join("workspace-a").join("project");
        let b_src = tmp.path().join("workspace-b").join("project");
        fs::create_dir_all(&a_src).unwrap();
        fs::create_dir_all(&b_src).unwrap();
        fs::write(a_src.join("main.cpp"), "int main() { return 0; }\n").unwrap();
        fs::write(b_src.join("main.cpp"), "int main() { return 0; }\n").unwrap();

        let a_watch = make_watch(
            &a_src,
            &tmp.path()
                .join("workspace-a")
                .join(".project.zccache_fp.json"),
        );
        let b_watch = make_watch(
            &b_src,
            &tmp.path()
                .join("workspace-b")
                .join(".project.zccache_fp.json"),
        );

        let a_hash = hash_watch_set_stamps(std::slice::from_ref(&a_watch)).unwrap();
        let b_hash = hash_watch_set_stamps(std::slice::from_ref(&b_watch)).unwrap();

        assert_eq!(a_hash, b_hash);
    }
}
