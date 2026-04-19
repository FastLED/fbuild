//! Shared warm-build fast path for platform orchestrators.
//!
//! The fast-path check lets an orchestrator skip its entire compile /
//! link pipeline when the previous build's metadata and watched input
//! files are byte-identical to the current invocation. It is the
//! critical lever that makes sub-100ms warm rebuilds possible for
//! FastLED (issue #121).
//!
//! The check has three layers, ordered by cost:
//! 1. **Metadata hash**: cheap string compare of the persisted
//!    per-build metadata hash (board, profile, toolchain dir, etc.).
//! 2. **Required artifacts**: stat each output file (firmware.elf,
//!    firmware.bin, compile_commands.json, ...) to make sure a
//!    previous build actually materialised its outputs.
//! 3. **Watched input set**: either the `zccache` daemon's
//!    persistent fingerprint (fastest — delegates the walk) or
//!    [`hash_watch_set_stamps_cached`], which is itself short-
//!    circuited by the daemon-scoped [`WatchSetStampCache`].
//!
//! If all three pass, the orchestrator can reuse the persisted
//! artifacts and size info. The helper hands back a
//! [`FastPathHit`] carrying the already-loaded
//! [`PersistedBuildFingerprint`] so the caller only has to re-use
//! fields, not reload them.

use std::path::{Path, PathBuf};

use fbuild_core::{Result, SizeInfo};

use super::{
    hash_watch_set_stamps_cached, load_json, PersistedBuildFingerprint, WatchSetStampCache,
    BUILD_FINGERPRINT_VERSION,
};
use crate::zccache::{self, FingerprintWatch};

/// File extensions considered source inputs for the watch-set fingerprint.
///
/// Covers C/C++ sources, headers, assembly, archives, linker scripts,
/// intermediate binaries, and common config files that influence a
/// build's output (Python build scripts, CSV partition tables, etc.).
pub const FAST_PATH_EXTENSIONS: &[&str] = &[
    "a", "bin", "c", "cc", "cpp", "csv", "elf", "h", "hh", "hpp", "ino", "json", "ld", "lds", "py",
    "s", "S", "txt",
];

/// Directories skipped during watch-set traversal.
///
/// These are either generated (build, target, .pio, .fbuild) or
/// developer-environment noise (.git, .venv, node_modules) that
/// should not invalidate a warm build.
pub const FAST_PATH_EXCLUDES: &[&str] = &[
    ".cache",
    ".fbuild",
    ".git",
    ".pio",
    ".venv",
    ".vscode",
    "__pycache__",
    "build",
    "node_modules",
    "target",
    "venv",
];

/// Build a default [`FingerprintWatch`] for a directory using the
/// shared fast-path extension / exclude lists.
///
/// Returns `None` if `root` does not exist, which lets callers skip
/// optional paths (e.g. a resolved-library tree that hasn't been
/// populated yet) without filtering in a second pass.
pub fn fast_path_watch(
    cache_name: &str,
    build_dir: &Path,
    root: &Path,
) -> Option<FingerprintWatch> {
    if !root.exists() {
        return None;
    }
    Some(FingerprintWatch {
        cache_file: build_dir.join(format!(".{}.zccache_fp.json", cache_name)),
        root: root.to_path_buf(),
        extensions: FAST_PATH_EXTENSIONS
            .iter()
            .map(|ext| (*ext).to_string())
            .collect(),
        excludes: FAST_PATH_EXCLUDES
            .iter()
            .map(|exclude| (*exclude).to_string())
            .collect(),
    })
}

/// Inputs to [`fast_path_check`].
///
/// Bundled in a struct so callers don't accumulate 8-argument calls
/// and so field additions stay source-compatible. All lifetimes tie
/// to the orchestrator's own call frame.
pub struct FastPathInputs<'a> {
    /// Location of the persisted `build_fingerprint.json`.
    pub fingerprint_path: &'a Path,
    /// Current build's metadata hash (board, profile, flash params, …).
    /// A mismatch against the persisted value forces a full rebuild.
    pub metadata_hash: &'a str,
    /// Directories walked to form the watch-set fingerprint.
    pub watches: &'a [FingerprintWatch],
    /// Files that MUST exist on disk for a cache hit to be valid
    /// (ELF, firmware binary, compile_commands.json, …).
    pub required_artifacts: &'a [PathBuf],
    /// Optional extra "is current?" callback run after the artifact
    /// existence check. ESP32 uses this to require that
    /// `compile_commands.json` also has the PlatformIO-style project-root
    /// copy. Returning `false` forces a full rebuild.
    pub extra_artifact_ok: Option<&'a dyn Fn() -> bool>,
    /// Optional daemon-scoped memo for the [`hash_watch_set_stamps`]
    /// walk (see [`WatchSetStampCache`]). `None` when invoked outside
    /// the daemon (tests, direct CLI).
    pub watch_set_cache: Option<&'a dyn WatchSetStampCache>,
    /// Discovered zccache binary, if any. When present the helper
    /// uses its persistent fingerprint as the primary invalidation
    /// signal and falls back to the watch-set hash only on zccache
    /// failure.
    pub compiler_cache: Option<&'a Path>,
}

/// Payload returned by a successful [`fast_path_check`].
///
/// Gives the caller what it needs to assemble a `BuildResult` without
/// re-reading the fingerprint or re-running the size analysis.
#[derive(Debug, Clone)]
pub struct FastPathHit {
    /// Full persisted fingerprint for the prior build.
    pub persisted: PersistedBuildFingerprint,
    /// Size info from the prior build (forwarded into the BuildResult).
    pub size_info: Option<SizeInfo>,
}

/// Check whether a prior build's artifacts can be reused for the
/// current invocation.
///
/// Returns:
/// - `Ok(Some(hit))` — all checks passed; the caller should short-
///   circuit and return a BuildResult using `hit.persisted` +
///   `hit.size_info` plus the artifact paths it tracks itself.
/// - `Ok(None)` — any check failed (no persisted fingerprint,
///   metadata mismatch, missing artifact, watched files changed).
///   The caller should do a full build.
/// - `Err(e)` — an I/O or hashing failure that the caller should
///   surface. Logging a warning and treating this as a miss is also
///   acceptable; callers that match existing ESP32 behaviour should
///   do so.
///
/// The helper itself logs parse/hash warnings via `tracing::warn!`
/// but never panics.
pub fn fast_path_check(inputs: &FastPathInputs<'_>) -> Result<Option<FastPathHit>> {
    // Load the persisted fingerprint. A parse error falls back to a
    // full build (matches the pre-extraction ESP32 behaviour).
    let persisted: Option<PersistedBuildFingerprint> =
        match load_json::<PersistedBuildFingerprint>(inputs.fingerprint_path) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!("ignoring invalid build fingerprint: {}", e);
                None
            }
        };

    let Some(previous) = persisted else {
        return Ok(None);
    };

    if previous.version != BUILD_FINGERPRINT_VERSION {
        return Ok(None);
    }
    if previous.metadata_hash != inputs.metadata_hash {
        return Ok(None);
    }

    // All declared artifacts must exist.
    for artifact in inputs.required_artifacts {
        if !artifact.exists() {
            return Ok(None);
        }
    }
    // Optional caller-supplied freshness hook (e.g. compile-db copy
    // parity between build_dir and project_dir on ESP32).
    if let Some(check) = inputs.extra_artifact_ok {
        if !check() {
            return Ok(None);
        }
    }

    // Watch-set fingerprint: prefer zccache's daemon fingerprint (it
    // already does an in-process walk cached across invocations).
    // On zccache miss/error, fall back to the recorded
    // `file_set_hash` using the in-memory WatchSetStampCache.
    let file_set_matches = if let Some(zcc) = inputs.compiler_cache {
        check_with_zccache(zcc, inputs.watches, &previous, inputs.watch_set_cache)
    } else {
        check_with_stamps(inputs.watches, &previous, inputs.watch_set_cache)
    };

    if !file_set_matches {
        return Ok(None);
    }

    Ok(Some(FastPathHit {
        size_info: previous.size_info.clone(),
        persisted: previous,
    }))
}

/// zccache-powered fingerprint check with graceful fallback.
fn check_with_zccache(
    zcc: &Path,
    watches: &[FingerprintWatch],
    previous: &PersistedBuildFingerprint,
    watch_set_cache: Option<&dyn WatchSetStampCache>,
) -> bool {
    let mut changed = false;
    let mut zccache_ok = true;
    for watch in watches {
        match zccache::check_fingerprint(zcc, watch) {
            Ok(zccache::FingerprintCheck::Unchanged) => {}
            Ok(zccache::FingerprintCheck::Changed) => {
                changed = true;
                break;
            }
            Err(e) => {
                tracing::warn!(
                    "zccache fingerprint unavailable for {}: {}",
                    watch.root.display(),
                    e
                );
                zccache_ok = false;
                break;
            }
        }
    }
    if zccache_ok {
        !changed
    } else {
        check_with_stamps(watches, previous, watch_set_cache)
    }
}

/// Hash-based fingerprint check against the persisted `file_set_hash`.
fn check_with_stamps(
    watches: &[FingerprintWatch],
    previous: &PersistedBuildFingerprint,
    watch_set_cache: Option<&dyn WatchSetStampCache>,
) -> bool {
    let Some(previous_hash) = previous.file_set_hash.as_deref() else {
        return false;
    };
    match hash_watch_set_stamps_cached(watches, watch_set_cache) {
        Ok(current_hash) => current_hash == previous_hash,
        Err(e) => {
            tracing::warn!("failed to hash watched inputs: {}", e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Mutex};

    /// Simple stamp cache used to prove the orchestrator wiring flows
    /// through the shared helper. Not what the daemon ships.
    #[derive(Default)]
    struct RecordingCache {
        entries: Mutex<Vec<(Vec<PathBuf>, String)>>,
    }

    impl WatchSetStampCache for RecordingCache {
        fn get(&self, watches: &[FingerprintWatch]) -> Option<String> {
            let key = key_for(watches);
            self.entries
                .lock()
                .unwrap()
                .iter()
                .find(|(k, _)| k == &key)
                .map(|(_, v)| v.clone())
        }

        fn put(&self, watches: &[FingerprintWatch], hash: String) {
            let key = key_for(watches);
            let mut entries = self.entries.lock().unwrap();
            entries.retain(|(k, _)| k != &key);
            entries.push((key, hash));
        }
    }

    fn key_for(watches: &[FingerprintWatch]) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = watches.iter().map(|w| w.root.clone()).collect();
        roots.sort();
        roots
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        fingerprint_path: PathBuf,
        required_artifact: PathBuf,
        src_root: PathBuf,
        watch: FingerprintWatch,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::TempDir::new().unwrap();
            let build_dir = tmp.path().join("build");
            fs::create_dir_all(&build_dir).unwrap();
            let src_root = tmp.path().join("src");
            fs::create_dir_all(&src_root).unwrap();
            let main = src_root.join("main.cpp");
            fs::write(&main, "int main() { return 0; }\n").unwrap();

            let fingerprint_path = build_dir.join("build_fingerprint.json");
            let required_artifact = build_dir.join("firmware.elf");
            fs::write(&required_artifact, b"elf-bytes").unwrap();

            let watch = super::fast_path_watch("project", &build_dir, &src_root)
                .expect("watch present — src_root exists");

            Self {
                _tmp: tmp,
                fingerprint_path,
                required_artifact,
                src_root,
                watch,
            }
        }

        fn write_fingerprint(&self, metadata_hash: &str) -> String {
            let file_set_hash =
                super::super::hash_watch_set_stamps(std::slice::from_ref(&self.watch)).unwrap();
            let fp = PersistedBuildFingerprint {
                version: BUILD_FINGERPRINT_VERSION,
                metadata_hash: metadata_hash.to_string(),
                file_set_hash: Some(file_set_hash.clone()),
                size_info: None,
            };
            super::super::save_json(&self.fingerprint_path, &fp).unwrap();
            file_set_hash
        }
    }

    #[test]
    fn fast_path_hits_when_inputs_unchanged() {
        let fx = Fixture::new();
        fx.write_fingerprint("meta-abc");

        let cache: Arc<RecordingCache> = Arc::new(RecordingCache::default());
        let required = vec![fx.required_artifact.clone()];
        let watches = vec![fx.watch.clone()];
        let inputs = FastPathInputs {
            fingerprint_path: &fx.fingerprint_path,
            metadata_hash: "meta-abc",
            watches: &watches,
            required_artifacts: &required,
            extra_artifact_ok: None,
            watch_set_cache: Some(cache.as_ref()),
            compiler_cache: None,
        };

        let hit = fast_path_check(&inputs).expect("check must not error on happy path");
        assert!(hit.is_some(), "expected fast-path hit");

        // Second call should populate the memoised watch-set cache so
        // a third call skips the walk entirely (can't observe the
        // skip directly without instrumentation, but the cache must
        // hold an entry).
        let _ = fast_path_check(&inputs).unwrap();
        let recorded = cache.entries.lock().unwrap().len();
        assert_eq!(recorded, 1, "watch-set cache should record one entry");
    }

    #[test]
    fn fast_path_misses_when_fingerprint_absent() {
        let fx = Fixture::new();
        // Deliberately do NOT write the fingerprint file.
        let required = vec![fx.required_artifact.clone()];
        let watches = vec![fx.watch.clone()];
        let inputs = FastPathInputs {
            fingerprint_path: &fx.fingerprint_path,
            metadata_hash: "meta-abc",
            watches: &watches,
            required_artifacts: &required,
            extra_artifact_ok: None,
            watch_set_cache: None,
            compiler_cache: None,
        };
        let hit = fast_path_check(&inputs).unwrap();
        assert!(hit.is_none(), "missing fingerprint must force a full build");
    }

    #[test]
    fn fast_path_misses_when_watch_set_changes() {
        let fx = Fixture::new();
        fx.write_fingerprint("meta-abc");

        // Touch a tracked source file to invalidate the stamp.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(
            fx.src_root.join("main.cpp"),
            "int main() { return 1; }\n// touched\n",
        )
        .unwrap();

        let required = vec![fx.required_artifact.clone()];
        let watches = vec![fx.watch.clone()];
        let inputs = FastPathInputs {
            fingerprint_path: &fx.fingerprint_path,
            metadata_hash: "meta-abc",
            watches: &watches,
            required_artifacts: &required,
            extra_artifact_ok: None,
            watch_set_cache: None,
            compiler_cache: None,
        };
        let hit = fast_path_check(&inputs).unwrap();
        assert!(
            hit.is_none(),
            "changed source file must invalidate fast path"
        );
    }

    #[test]
    fn fast_path_misses_when_metadata_hash_changes() {
        let fx = Fixture::new();
        fx.write_fingerprint("meta-abc");

        let required = vec![fx.required_artifact.clone()];
        let watches = vec![fx.watch.clone()];
        let inputs = FastPathInputs {
            fingerprint_path: &fx.fingerprint_path,
            metadata_hash: "meta-xyz",
            watches: &watches,
            required_artifacts: &required,
            extra_artifact_ok: None,
            watch_set_cache: None,
            compiler_cache: None,
        };
        let hit = fast_path_check(&inputs).unwrap();
        assert!(hit.is_none(), "metadata hash mismatch must invalidate");
    }

    #[test]
    fn fast_path_misses_when_required_artifact_missing() {
        let fx = Fixture::new();
        fx.write_fingerprint("meta-abc");
        fs::remove_file(&fx.required_artifact).unwrap();

        let required = vec![fx.required_artifact.clone()];
        let watches = vec![fx.watch.clone()];
        let inputs = FastPathInputs {
            fingerprint_path: &fx.fingerprint_path,
            metadata_hash: "meta-abc",
            watches: &watches,
            required_artifacts: &required,
            extra_artifact_ok: None,
            watch_set_cache: None,
            compiler_cache: None,
        };
        let hit = fast_path_check(&inputs).unwrap();
        assert!(hit.is_none(), "missing artifact must invalidate");
    }

    #[test]
    fn fast_path_respects_extra_artifact_ok_callback() {
        let fx = Fixture::new();
        fx.write_fingerprint("meta-abc");

        let required = vec![fx.required_artifact.clone()];
        let watches = vec![fx.watch.clone()];
        let always_stale = || false;
        let inputs = FastPathInputs {
            fingerprint_path: &fx.fingerprint_path,
            metadata_hash: "meta-abc",
            watches: &watches,
            required_artifacts: &required,
            extra_artifact_ok: Some(&always_stale),
            watch_set_cache: None,
            compiler_cache: None,
        };
        let hit = fast_path_check(&inputs).unwrap();
        assert!(
            hit.is_none(),
            "extra_artifact_ok returning false must invalidate"
        );
    }
}
