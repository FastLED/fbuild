//! On-demand, budgeted cache reclaim (FastLED/fbuild#1750 item 3).
//!
//! A full disk must be fixable by an explicit command, never as a build side
//! effect. #1341 is the cautionary tale: the daemon's background GC evicted a
//! warm toolchain *mid-build* and the link step died on `ENOENT`. So this
//! module is the opposite of the GC loop — it runs only when asked, it evicts
//! in a fixed order, it stops as soon as the requested byte count is free, and
//! it reports every byte it took. The callers own the "never mid-build" guard
//! (`fbuild cache reclaim` refuses while the daemon reports an operation in
//! progress).
//!
//! Eviction order, cheapest-to-refetch first (the order #1750 asks for):
//!
//! 1. [`Kind::Toolchains`] — warm toolchain / esptool installs, a download.
//! 2. [`Kind::Packages`] — platform packages, a download.
//! 3. [`Kind::Platforms`] — platform sources, a download.
//! 4. [`Kind::Libraries`] — resolved library trees, a download.
//! 5. [`Kind::Frameworks`] — framework libs, re-hydrated from the cache.
//! 6. [`Kind::LnkBlobs`] — content-addressed blobs shared across URLs, last.
//!
//! Within a kind the extracted install goes before the archive, exactly like
//! [`super::gc`]: reinstalling is cheaper than re-downloading. Both halves keep
//! the #20 safety model — pinned and leased entries are invisible to reclaim
//! (the queries filter `pinned = 0`), so a build that is already holding an
//! entry cannot lose it.
//!
//! The zccache object cache is the one step this module does *not* evict: its
//! LRU budget belongs to the embedded zccache service inside `fbuild-daemon`,
//! and there is no trim API across the process boundary. Callers report that
//! step instead of half-doing it.

use super::index::{CacheEntry, CacheIndex};
use super::paths::Kind;

/// The fixed eviction order (see the module docs). Never reorder to chase a
/// bigger number: the ordering is what keeps a reclaim from deleting the one
/// thing the next build needs most.
pub const RECLAIM_ORDER: [Kind; 6] = [
    Kind::Toolchains,
    Kind::Packages,
    Kind::Platforms,
    Kind::Libraries,
    Kind::Frameworks,
    Kind::LnkBlobs,
];

/// Most entries reclaim will consider per phase per kind in one run. The order
/// is fixed and the target is bounded, so a host with more than this many live
/// entries of a single kind is already far outside the sizes #1750 measured.
pub const RECLAIM_BATCH: usize = 10_000;

/// Which half of an entry is being removed.
#[derive(Debug, Clone, Copy)]
enum Phase {
    Installed,
    Archive,
}

/// Bytes evicted for one cache kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimStep {
    pub kind: Kind,
    pub installed_evicted: u64,
    pub installed_bytes_freed: u64,
    pub archives_evicted: u64,
    pub archive_bytes_freed: u64,
}

impl ReclaimStep {
    /// A step with nothing evicted yet.
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            installed_evicted: 0,
            installed_bytes_freed: 0,
            archives_evicted: 0,
            archive_bytes_freed: 0,
        }
    }

    /// Bytes this step freed (or, in a dry run, would free).
    pub fn freed_bytes(&self) -> u64 {
        self.installed_bytes_freed + self.archive_bytes_freed
    }

    /// Entries this step evicted (or, in a dry run, would evict).
    pub fn evicted(&self) -> u64 {
        self.installed_evicted + self.archives_evicted
    }

    fn touched(&self) -> bool {
        self.evicted() > 0 || self.freed_bytes() > 0
    }
}

/// What a reclaim run did, in eviction order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimReport {
    needed_bytes: u64,
    steps: Vec<ReclaimStep>,
    dry_run: bool,
}

impl ReclaimReport {
    /// An empty report for `needed_bytes`. Public so a renderer (and its
    /// tests) can be driven with fabricated steps instead of real files.
    pub fn new(needed_bytes: u64, dry_run: bool) -> Self {
        Self {
            needed_bytes,
            steps: Vec::new(),
            dry_run,
        }
    }

    /// The byte count reclaim was asked to free.
    pub fn needed_bytes(&self) -> u64 {
        self.needed_bytes
    }

    /// Append one step to the report.
    pub fn push_step(&mut self, step: ReclaimStep) {
        self.steps.push(step);
    }

    /// Steps that evicted something, in eviction order.
    pub fn steps(&self) -> &[ReclaimStep] {
        &self.steps
    }

    /// Bytes freed across every step (would be freed, in a dry run).
    pub fn freed_bytes(&self) -> u64 {
        self.steps.iter().map(ReclaimStep::freed_bytes).sum()
    }

    /// Entries evicted across every step.
    pub fn evicted(&self) -> u64 {
        self.steps.iter().map(ReclaimStep::evicted).sum()
    }

    pub fn dry_run(&self) -> bool {
        self.dry_run
    }

    /// Whether the run freed at least [`needed_bytes`](Self::needed_bytes).
    pub fn is_satisfied(&self) -> bool {
        self.freed_bytes() >= self.needed_bytes
    }

    /// Bytes still missing after the run (zero when satisfied).
    pub fn remaining_bytes(&self) -> u64 {
        self.needed_bytes.saturating_sub(self.freed_bytes())
    }
}

/// Free `needed_bytes` by evicting disposable caches in [`RECLAIM_ORDER`],
/// stopping as soon as the target is met.
///
/// `dry_run` walks exactly the same order and arithmetic but deletes nothing,
/// so `--dry-run` and the real run report the same numbers on an unchanged
/// cache.
///
/// A remove that fails (busy file, permission) is logged and skipped rather
/// than aborting the walk: reclaim is a best-effort recovery path and a single
/// locked file must not leave the host with no room. Such an entry keeps its
/// index row, so the next run retries it.
pub fn reclaim(
    index: &CacheIndex,
    needed_bytes: u64,
    dry_run: bool,
) -> rusqlite::Result<ReclaimReport> {
    let mut report = ReclaimReport::new(needed_bytes, dry_run);
    if needed_bytes == 0 {
        return Ok(report);
    }

    let mut freed = 0u64;
    for kind in RECLAIM_ORDER {
        if freed >= needed_bytes {
            break;
        }
        let mut step = ReclaimStep::new(kind);

        // Extracted installs first: cheaper to rehydrate than to re-download.
        for entry in index.lru_installed_entries_of_kind(kind, RECLAIM_BATCH)? {
            if freed >= needed_bytes {
                break;
            }
            let bytes = entry.installed_bytes.unwrap_or(0).max(0) as u64;
            if !dry_run && !evict_installed(index, &entry) {
                continue;
            }
            freed += bytes;
            step.installed_evicted += 1;
            step.installed_bytes_freed += bytes;
        }

        for entry in index.lru_archive_entries_of_kind(kind, RECLAIM_BATCH)? {
            if freed >= needed_bytes {
                break;
            }
            let bytes = entry.archive_bytes.unwrap_or(0).max(0) as u64;
            if !dry_run && !evict_archive(index, &entry) {
                continue;
            }
            freed += bytes;
            step.archives_evicted += 1;
            step.archive_bytes_freed += bytes;
        }

        if step.touched() {
            report.push_step(step);
        }
    }
    Ok(report)
}

/// Remove an entry's installed directory and null the column. `false` means the
/// entry is still on disk (nothing was counted).
fn evict_installed(index: &CacheIndex, entry: &CacheEntry) -> bool {
    if let Some(path) = entry.installed_path.as_ref() {
        if !remove_path(&index.cache_root().join(path), Phase::Installed) {
            return false;
        }
    }
    if let Err(error) = index.clear_installed(entry.id) {
        tracing::warn!(
            "reclaim: failed to clear installed row {}: {error}",
            entry.id
        );
        return false;
    }
    drop_empty_row(index, entry.id, entry.archive_path.is_none());
    true
}

/// Remove an entry's archive file and null the column. Same contract as
/// [`evict_installed`].
fn evict_archive(index: &CacheIndex, entry: &CacheEntry) -> bool {
    if let Some(path) = entry.archive_path.as_ref() {
        if !remove_path(&index.cache_root().join(path), Phase::Archive) {
            return false;
        }
    }
    if let Err(error) = index.clear_archive(entry.id) {
        tracing::warn!("reclaim: failed to clear archive row {}: {error}", entry.id);
        return false;
    }
    drop_empty_row(index, entry.id, entry.installed_path.is_none());
    true
}

/// Delete a path; `NotFound` counts as success (someone already removed it).
fn remove_path(path: &std::path::Path, phase: Phase) -> bool {
    let result = match phase {
        Phase::Installed => std::fs::remove_dir_all(path),
        Phase::Archive => std::fs::remove_file(path),
    };
    match result {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!("reclaim: failed to remove {}: {error}", path.display());
            false
        }
    }
}

/// Delete an index row whose last phase was just cleared.
fn drop_empty_row(index: &CacheIndex, entry_id: i64, is_empty: bool) {
    if is_empty {
        if let Err(error) = index.delete_entry(entry_id) {
            tracing::warn!("reclaim: failed to delete row {entry_id}: {error}");
        }
    }
}

#[cfg(test)]
#[path = "reclaim_tests.rs"]
mod tests;
