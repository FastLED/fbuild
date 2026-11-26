//! Disk-cache LRU for prebuilt trees (#1882).
//!
//! [`obtain`](super::obtain) commits one tree per exact identity under
//! `{cache}/prebuilt/`, and nothing ever deleted them: a client that walks
//! framework versions accumulates a full decompressed framework per fingerprint
//! forever. [`sweep_staging_tree`](super::sweep_staging_tree) does not help — it
//! reaps abandoned *staging* attempts and deliberately never touches a tree
//! carrying the completion marker.
//!
//! This cell owns the eviction decision, and only that:
//!
//! - a candidate is a directory below `{cache}/prebuilt/` holding a regular
//!   [`MARKER`] file — a marker is written only after the whole tree verified,
//!   so a staging directory or a partial rename is never a candidate (the sweep
//!   owns those), and the prebuilt root itself is never a candidate;
//! - recency is the marker's modification time, and
//!   [`touch`] bumps it when a tree is *reused*, so the order reflects last use
//!   rather than creation — a tree that keeps being reused is never evicted;
//! - a tree whose marker is younger than the grace period is kept and counted:
//!   a build may be reading it right now, and there are no leases on this path;
//! - eviction is oldest-first, bounded by [`MAX_REMOVALS`], and stops as soon as
//!   the remaining bytes fit the budget;
//! - nothing outside the prebuilt root is read, a symlinked directory is never
//!   entered or removed, and a removal error is reported rather than hidden.
//!
//! Opt-in: [`declared_budget`] returns `None` unless the operator sets
//! `FBUILD_PREBUILT_BUDGET`, and the daemon then calls [`prune`] on the same 6 h
//! schedule as the sweep. An unset variable means the LRU never runs, so a cache
//! that predates this cell is never pruned by surprise.
use super::MARKER;
use fbuild_core::path::NormalizedPath;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Environment variable that declares the byte budget for prebuilt trees.
pub const BUDGET_ENV: &str = "FBUILD_PREBUILT_BUDGET";
/// Most trees one prune removes, so a run's work stays bounded.
pub const MAX_REMOVALS: usize = 8;
/// Most directories one scan visits, so a wide cache stays bounded.
pub const MAX_SCAN_DIRS: usize = super::gc::MAX_SCAN_DIRS;
/// Deepest level below the prebuilt root a scan descends into.
pub const MAX_SCAN_DEPTH: usize = super::gc::MAX_SCAN_DEPTH;

#[derive(Debug, thiserror::Error)]
pub enum LruError {
    #[error("{path}: {error}")]
    Io { path: String, error: String },
    #[error("prebuilt tree LRU task failed: {0}")]
    Join(String),
}

/// One reusable tree: where it is, how big, and when it was last used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeUse {
    pub root: NormalizedPath,
    pub bytes: u64,
    /// Marker modification time: extraction time, or the last reuse.
    pub last_used: SystemTime,
}

/// What one prune measured and what it deliberately left alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictReport {
    /// Trees found below the prebuilt root.
    pub trees: usize,
    /// Bytes all found trees hold.
    pub scanned_bytes: u64,
    /// Bytes the budget allowed.
    pub budget_bytes: u64,
    /// Trees removed by this prune.
    pub removed: usize,
    /// Bytes those trees held, as measured before removal.
    pub freed_bytes: u64,
    /// Trees kept because their marker is younger than the grace period.
    pub kept_live: usize,
    /// Bytes left after the prune.
    pub remaining_bytes: u64,
}

/// The operator-declared budget, or `None` when the LRU is not enabled.
///
/// Unset or empty disables the LRU; an unparseable value also disables it rather
/// than guessing, and `0` is a real budget that evicts every eligible tree.
pub fn declared_budget() -> Option<u64> {
    budget_from(std::env::var(BUDGET_ENV).ok().as_deref())
}

/// [`declared_budget`] on a raw value, so the gate is testable without the
/// process environment.
pub fn budget_from(raw: Option<&str>) -> Option<u64> {
    crate::disk_cache::budget::parse_human_size(raw?)
}

/// Every reusable tree below `{cache_root}/prebuilt/`, unsorted.
pub fn trees(cache_root: &Path) -> Result<Vec<TreeUse>, LruError> {
    let root = NormalizedPath::new(crate::disk_cache::paths::prebuilt_root(cache_root));
    let mut found = Vec::new();
    if !is_real_dir(root.as_path()) {
        return Ok(found);
    }
    for entry in walkdir::WalkDir::new(root.as_path())
        .max_depth(MAX_SCAN_DEPTH)
        .follow_links(false)
    {
        if found.len() >= MAX_SCAN_DIRS {
            break;
        }
        let entry = entry.map_err(|error| io_error(root.as_path(), error.into()))?;
        if !entry.file_type().is_dir() || entry.depth() == 0 {
            continue;
        }
        let tree = NormalizedPath::new(entry.path());
        let Some(last_used) = marker_time(&tree) else {
            continue;
        };
        found.push(TreeUse {
            bytes: super::gc::tree_bytes(tree.as_path()),
            last_used,
            root: tree,
        });
    }
    Ok(found)
}

/// Evict the least-recently-used trees until the prebuilt root fits `budget`.
pub fn prune(cache_root: &Path, budget: u64, min_age: Duration) -> Result<EvictReport, LruError> {
    prune_at(cache_root, budget, min_age, SystemTime::now())
}

/// [`prune`] on its own thread, for callers on the daemon runtime.
pub async fn prune_async(
    cache_root: NormalizedPath,
    budget: u64,
    min_age: Duration,
) -> Result<EvictReport, LruError> {
    tokio::task::spawn_blocking(move || prune(cache_root.as_path(), budget, min_age))
        .await
        .map_err(|error| LruError::Join(error.to_string()))?
}

/// [`prune`] with an explicit clock, so the grace period is testable without
/// touching file times.
fn prune_at(
    cache_root: &Path,
    budget: u64,
    min_age: Duration,
    now: SystemTime,
) -> Result<EvictReport, LruError> {
    let mut found = trees(cache_root)?;
    // Oldest first; the path breaks ties so one cache always prunes the same
    // trees in the same order.
    found.sort_by(|left, right| {
        left.last_used
            .cmp(&right.last_used)
            .then_with(|| left.root.as_path().cmp(right.root.as_path()))
    });
    let scanned = found
        .iter()
        .fold(0_u64, |sum, tree| sum.saturating_add(tree.bytes));
    let mut report = EvictReport {
        trees: found.len(),
        scanned_bytes: scanned,
        budget_bytes: budget,
        remaining_bytes: scanned,
        ..EvictReport::default()
    };
    for tree in found {
        if report.remaining_bytes <= budget || report.removed >= MAX_REMOVALS {
            break;
        }
        // A marker in the future yields the zero age, which keeps the tree:
        // guessing "old" is the unsafe direction.
        if now.duration_since(tree.last_used).unwrap_or_default() < min_age {
            report.kept_live += 1;
            continue;
        }
        std::fs::remove_dir_all(tree.root.as_path())
            .map_err(|error| io_error(tree.root.as_path(), error))?;
        report.removed += 1;
        report.freed_bytes = report.freed_bytes.saturating_add(tree.bytes);
        report.remaining_bytes = report.remaining_bytes.saturating_sub(tree.bytes);
    }
    Ok(report)
}

/// Record a use of the tree at `tree_root`, so the LRU sees last use, not
/// creation.
///
/// A missing tree is [`Existing::None`] and is not an error: the caller reused a
/// tree that a concurrent prune had already removed, and the next resolve simply
/// downloads it again.
pub fn touch(tree_root: &Path, now: SystemTime) -> Result<(), LruError> {
    if !has_marker(tree_root) {
        return Ok(());
    }
    let marker = NormalizedPath::new(tree_root).join(MARKER);
    let file = std::fs::File::options()
        .write(true)
        .open(marker.as_path())
        .map_err(|error| io_error(marker.as_path(), error))?;
    let times = std::fs::FileTimes::new().set_modified(now);
    file.set_times(times)
        .map_err(|error| io_error(marker.as_path(), error))
}

/// [`touch`] on its own thread, for callers on the daemon runtime.
pub async fn touch_async(tree_root: NormalizedPath, now: SystemTime) -> Result<(), LruError> {
    tokio::task::spawn_blocking(move || touch(tree_root.as_path(), now))
        .await
        .map_err(|error| LruError::Join(error.to_string()))?
}

/// A marker that is a regular file, not a link, and its modification time.
fn marker_time(tree: &NormalizedPath) -> Option<SystemTime> {
    let marker = tree.join(MARKER);
    let metadata = std::fs::symlink_metadata(marker.as_path()).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    metadata.modified().ok()
}

fn has_marker(tree_root: &Path) -> bool {
    marker_time(&NormalizedPath::new(tree_root)).is_some()
}

fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn io_error(path: &Path, error: std::io::Error) -> LruError {
    LruError::Io {
        path: path.display().to_string(),
        error: error.to_string(),
    }
}

#[cfg(test)]
#[path = "lru_tests.rs"]
mod tests;
