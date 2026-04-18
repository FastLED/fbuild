//! Daemon-scoped in-memory cache for `hash_watch_set_stamps` results.
//!
//! Implements [`fbuild_build::build_fingerprint::WatchSetStampCache`]
//! over a `DashMap` keyed by a stable hash of the watch set's root
//! paths. Entries are invalidated by a freshness window so a long-
//! running daemon doesn't serve a stale "no changes" answer to a
//! warm-rebuild call that comes minutes after the last one.
//!
//! # Why
//!
//! `hash_watch_set_stamps` walks every file under each watch root and
//! stat()s it to build a per-build fingerprint. On large projects
//! (FastLED-class sketches with the Arduino framework + libraries),
//! that walk is the dominant cost on warm rebuilds — see
//! `docs/PERF_WARM_BUILD.md`.
//!
//! Within the same daemon lifetime a back-to-back `fbuild build` /
//! `fbuild deploy` round-trip can reuse the previous walk's result if
//! it's only a few seconds old: any source change a user just made
//! arrived through the file system, which already advanced the watch
//! root's mtime — but our heuristic deliberately doesn't try to be
//! that precise. A short freshness window (default 2 s, see
//! [`DEFAULT_FRESHNESS`]) is enough for the warm-loop case while
//! keeping the worst-case "ignored a real change" window human-noticeable.
//!
//! # Cycle / staleness model
//!
//! - Cache key: stable u64 derived from sorted watch root paths.
//! - Cache value: `(hash, set_at: Instant)`.
//! - Hit when `entry.set_at.elapsed() < max_age`.
//! - Miss otherwise — the orchestrator falls through to the real walk
//!   and stores the new result.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use fbuild_build::build_fingerprint::WatchSetStampCache;
use fbuild_build::zccache::FingerprintWatch;

/// Default freshness window for cache entries. Short enough that a
/// user editing a file and immediately re-building still triggers
/// the real walk (modulo edit speed), long enough to cover the
/// back-to-back deploy / re-deploy interaction the sub-1 s budget
/// targets. Override per-instance via [`DaemonWatchSetCache::with_max_age`].
pub const DEFAULT_FRESHNESS: Duration = Duration::from_secs(2);

/// In-memory cache. Cheap to clone via `Arc` because the only
/// state is a `DashMap`.
pub struct DaemonWatchSetCache {
    inner: DashMap<u64, (String, Instant)>,
    max_age: Duration,
}

impl Default for DaemonWatchSetCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DaemonWatchSetCache {
    pub fn new() -> Self {
        Self::with_max_age(DEFAULT_FRESHNESS)
    }

    pub fn with_max_age(max_age: Duration) -> Self {
        Self {
            inner: DashMap::new(),
            max_age,
        }
    }

    /// Number of currently-stored entries. Test-only: production
    /// callers shouldn't care, the cache is opaque.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the cache has any stored entries. Test-only —
    /// `len == 0` equivalent; exists so clippy's
    /// `len_without_is_empty` is satisfied.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl WatchSetStampCache for DaemonWatchSetCache {
    fn get(&self, watches: &[FingerprintWatch]) -> Option<String> {
        let key = key_for(watches);
        let entry = self.inner.get(&key)?;
        let (hash, set_at) = (entry.0.clone(), entry.1);
        drop(entry);
        if set_at.elapsed() >= self.max_age {
            // Lazy eviction so a stale entry doesn't keep memory
            // pinned indefinitely; the next put would have replaced
            // it anyway, but explicit removal helps a long-idle
            // daemon.
            self.inner.remove(&key);
            return None;
        }
        Some(hash)
    }

    fn put(&self, watches: &[FingerprintWatch], hash: String) {
        let key = key_for(watches);
        self.inner.insert(key, (hash, Instant::now()));
    }
}

/// Stable key derived from the watch set's root paths. We sort
/// before hashing so the orchestrator can hand us watches in any
/// order without changing the key.
fn key_for(watches: &[FingerprintWatch]) -> u64 {
    let mut roots: Vec<&std::path::Path> = watches.iter().map(|w| w.root.as_path()).collect();
    roots.sort();
    let mut h = DefaultHasher::new();
    for r in roots {
        r.hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn watch(root: &str) -> FingerprintWatch {
        FingerprintWatch {
            cache_file: PathBuf::from(format!("{root}/cache.json")),
            root: PathBuf::from(root),
            extensions: vec!["c".to_string()],
            excludes: vec![],
        }
    }

    /// Round-trip: put a hash, get it back inside the freshness
    /// window. Two distinct watch sets must not collide.
    #[test]
    fn put_then_get_returns_same_hash() {
        let cache = DaemonWatchSetCache::new();
        let ws_a = vec![watch("/a")];
        let ws_b = vec![watch("/b")];
        cache.put(&ws_a, "AAA".to_string());
        cache.put(&ws_b, "BBB".to_string());
        assert_eq!(cache.get(&ws_a).as_deref(), Some("AAA"));
        assert_eq!(cache.get(&ws_b).as_deref(), Some("BBB"));
    }

    /// Same set of paths in different order hashes to the same key —
    /// orchestrator can hand us watches without sorting.
    #[test]
    fn key_is_order_insensitive() {
        let cache = DaemonWatchSetCache::new();
        let ws_ab = vec![watch("/a"), watch("/b")];
        let ws_ba = vec![watch("/b"), watch("/a")];
        cache.put(&ws_ab, "X".to_string());
        assert_eq!(cache.get(&ws_ba).as_deref(), Some("X"));
    }

    /// An entry older than `max_age` is treated as a miss and lazily
    /// evicted. We use a near-zero `max_age` so the entry is stale
    /// the moment we read it back.
    #[test]
    fn stale_entry_is_evicted() {
        let cache = DaemonWatchSetCache::with_max_age(Duration::from_millis(1));
        let ws = vec![watch("/x")];
        cache.put(&ws, "old".to_string());
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get(&ws).is_none());
        assert_eq!(cache.len(), 0, "stale entry should be evicted on get");
    }

    /// Unknown watch set returns `None` and doesn't fabricate a value.
    #[test]
    fn miss_returns_none() {
        let cache = DaemonWatchSetCache::new();
        let ws = vec![watch("/never-stored")];
        assert!(cache.get(&ws).is_none());
    }
}
