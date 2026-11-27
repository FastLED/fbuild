//! Daemon-side staging GC for extracted prebuilt framework trees (#1882).
//!
//! [`fbuild_packages::prebuilt::extract`] builds a tree in a
//! `.<sha16>.<attempt>.staging` sibling of its destination and removes it on
//! every ordinary error path. A process killed outright (SIGKILL, the OOM
//! killer, a reboot) cannot run that cleanup, so the directory survives holding
//! up to the decompressed size ceiling — one abandoned attempt per kill.
//!
//! The reaping and eviction rules live in `fbuild_packages::prebuilt`; this
//! module is the call site. The budget-capped LRU ([`prune_once`]) runs on the
//! same schedule and only when the operator declares `FBUILD_PREBUILT_BUDGET`.
//! Decision 4 of `docs/design/artifact-serving-decisions.md` makes the sweep
//! daemon-owned: **on daemon startup and every 6 h, on a blocking
//! task that never touches the request path** — the same shape the package
//! cache's own background GC already has (FastLED/fbuild#808).

use fbuild_core::path::NormalizedPath;
use fbuild_packages::prebuilt::{
    EvictReport, GcError, LruError, MIN_AGE, SweepReport, declared_budget, prune_async,
    sweep_staging_tree_async,
};
use std::path::Path;
use std::time::Duration;

/// Interval between sweeps once the daemon is up (decision 4: "every 6 h").
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Sweep the prebuilt tree root under `cache_root` once.
///
/// Nothing is logged for a quiet sweep: on a healthy cache every pass finds no
/// candidates, and this runs four times a day on every install.
pub async fn sweep_once(cache_root: &Path) -> Result<SweepReport, GcError> {
    let root = NormalizedPath::new(cache_root);
    let report = sweep_staging_tree_async(root, MIN_AGE).await?;
    if report.removed > 0 || report.kept_marked > 0 {
        tracing::info!(
            "prebuilt staging sweep: removed {} abandoned attempt(s), {} freed, kept {} marked + {} live",
            report.removed,
            format_bytes(report.freed_bytes),
            report.kept_marked,
            report.kept_live,
        );
    }
    Ok(report)
}

/// Evict the least-recently-used trees down to `budget`.
///
/// The budget is the operator's: nothing here guesses one, and a `None` from
/// [`declared_budget`] means the LRU never runs. The pass is bounded by the
/// module's own removal ceiling and keeps every tree inside the grace period,
/// so a build reading a fresh tree is never evicted from under it.
pub async fn prune_once(cache_root: &Path, budget: u64) -> Result<EvictReport, LruError> {
    let root = NormalizedPath::new(cache_root);
    let report = prune_async(root, budget, MIN_AGE).await?;
    if report.removed > 0 || report.kept_live > 0 {
        tracing::info!(
            "prebuilt LRU: removed {} of {} tree(s), {} freed, {} kept live, {} left of {}",
            report.removed,
            report.trees,
            format_bytes(report.freed_bytes),
            report.kept_live,
            format_bytes(report.remaining_bytes),
            format_bytes(report.budget_bytes),
        );
    }
    Ok(report)
}

/// Spawn the sweep: once at startup, then every [`SWEEP_INTERVAL`].
///
/// The walk runs on a blocking task (`sweep_staging_tree_async`), so a slow or
/// very large cache never parks a tokio worker, and no sweep touches the
/// request path.
pub fn spawn_maintenance() {
    tokio::spawn(async move {
        let cache_root = fbuild_paths::get_cache_root();
        // Resolved once: an unset budget disables the LRU for the whole run.
        let budget = declared_budget();
        loop {
            if let Err(error) = sweep_once(cache_root.as_path()).await {
                tracing::warn!("prebuilt staging sweep failed: {}", error);
            }
            if let Some(budget) = budget {
                if let Err(error) = prune_once(cache_root.as_path(), budget).await {
                    tracing::warn!("prebuilt LRU prune failed: {}", error);
                }
            }
            tokio::time::sleep(SWEEP_INTERVAL).await;
        }
    });
}

/// Human byte count for the log line, matching the cache GC's wording.
fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    match bytes {
        b if b >= GIB => format!("{:.1} GiB", b as f64 / GIB as f64),
        b if b >= MIB => format!("{:.1} MiB", b as f64 / MIB as f64),
        b if b >= KIB => format!("{:.1} KiB", b as f64 / KIB as f64),
        b => format!("{b} B"),
    }
}

#[cfg(test)]
#[path = "prebuilt_gc_tests.rs"]
mod tests;
