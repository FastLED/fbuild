//! `fbuild doctor` — read-only host diagnostics.
//!
//! The one check today is `--check-disk` (FastLED/fbuild#1750 item 5): report
//! the free space of every root a cold ESP32 build writes, and how many bytes
//! the cache root could give back, **without building anything**. It reuses
//! `fbuild_build_engine::disk_preflight`, so the required byte counts, the
//! shortfall arithmetic and the reclaimable-cache listing are exactly the ones
//! a build acts on — `doctor` is the same measurement, reported instead of
//! enforced.
//!
//! It also reports the wheel-build cargo target store under the home directory
//! (FastLED/fbuild#2062): every `pip install` of a checkout creates one full
//! release target dir there and nothing else reclaims it, so the store grows one
//! per worktree ever created. `--reclaim-targets` is the one mutating path -- the
//! cron entry point -- and it only deletes dead target dirs inside the store.
//!
//! Nothing else mutates state: no daemon round-trip, no cache eviction, and an
//! unmeasurable volume is a warning line rather than a failure (an unreadable
//! `statvfs` must not look like a full disk). The exit code carries the
//! verdict so CI can gate on it.
//!
//! ```text
//! fbuild doctor --check-disk
//! fbuild doctor --check-disk ../my-project
//! fbuild doctor --check-disk --reclaim-targets   # cron: reclaim dead targets
//! ```

use std::path::Path;
use std::time::SystemTime;

use clap::Args;
use fbuild_build::build_output::format_bytes;
use fbuild_build::disk_preflight::{
    FreeSpace, HostFreeSpace, PreflightReport, ReclaimableCache, measure, reclaimable_caches,
};
use fbuild_build::esp32::orchestrator::disk_requirements::{
    MIN_DISK_ENV, cache_requirement, disk_roots,
};
use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};
use fbuild_paths::wheel_targets::{self, ReclaimedTarget, StoreSurvey};

use crate::output;

/// `fbuild doctor [--check-disk] [PROJECT_DIR]`.
#[derive(Args)]
#[command(about = "Read-only host diagnostics")]
pub struct DoctorArgs {
    /// Report free space on every root a build writes, plus the reclaimable
    /// cache total. Currently the only check, so `fbuild doctor` runs it too.
    #[arg(long)]
    pub check_disk: bool,

    /// Project directory whose build root is measured. Defaults to the current
    /// directory.
    #[arg(value_name = "PROJECT_DIR")]
    pub project_dir: Option<String>,

    /// Reclaim wheel-build cargo target dirs whose owning checkout is gone (or
    /// whose files are all older than the idle gate) -- the cron entry point.
    /// Deletes only inside the store.
    #[arg(long)]
    pub reclaim_targets: bool,
}

/// A rendered `--check-disk` report and its verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskCheck {
    pub text: String,
    pub ok: bool,
}

/// Entry point for `fbuild doctor` (FastLED/fbuild#1750 item 5).
///
/// Without `--check-disk` this runs the same check: disk space is the only
/// diagnostic fbuild has today, and a doctor that prints nothing is worse than
/// one that prints the one thing it knows.
pub fn run_doctor(args: DoctorArgs) -> Result<()> {
    let project_dir = project_dir_to_measure(args.project_dir.as_deref());
    let cache_bytes = cache_requirement();
    let cache_root = fbuild_paths::get_cache_root();
    let disabled = cache_bytes == 0;

    let roots = disk_roots(&project_dir, cache_bytes);
    let report = measure(&HostFreeSpace as &dyn FreeSpace, &roots);
    // A disabled check measures nothing, so walking the cache root for
    // reclaimable bytes would be the only work done — and it is the expensive
    // part (a full `walkdir`). Skip it.
    let reclaimable = if disabled {
        Vec::new()
    } else {
        reclaimable_caches(&cache_root)
    };

    let check = render_disk_check(&report, &reclaimable, &cache_root, disabled);
    let targets = wheel_build_targets_report(args.reclaim_targets);
    output::result(format!("{}\n{targets}", check.text.trim_end()).trim_end());
    if check.ok {
        return Ok(());
    }
    Err(FbuildError::BuildFailed(format!(
        "disk check failed: {} of {} measured root(s) short of space",
        report.short_roots().count(),
        report.roots().len()
    )))
}

/// Report the wheel-build cargo target store, reclaiming dead entries when
/// `reclaim` is set (FastLED/fbuild#2062 item 3).
///
/// A target dir is one full release build that nothing else in fbuild removes,
/// so an unreclaimed store grows one per worktree ever created. `reclaim` only
/// ever runs from an explicit flag -- `doctor` stays read-only otherwise.
pub fn wheel_build_targets_report(reclaim: bool) -> String {
    let idle_days = wheel_build_idle_days();
    let now = SystemTime::now();
    let Some(store) = wheel_targets::store_root() else {
        return "wheel-build cargo targets: could not determine the home directory\n".to_string();
    };
    let measured = wheel_targets::survey(store.as_path(), idle_days, now);
    let mut text = render_wheel_targets(&measured, idle_days);
    if reclaim {
        let reclaimed = wheel_targets::reclaim(store.as_path(), idle_days, now);
        text.push_str(&render_reclaimed(&reclaimed));
    }
    text
}

/// The idle gate for unstamped target dirs, from the environment a wheel build
/// uses. A bad value falls back to the documented default.
pub fn wheel_build_idle_days() -> f64 {
    const IDLE_DAYS_ENV: &str = "FBUILD_WHEEL_BUILD_IDLE_DAYS";
    std::env::var(IDLE_DAYS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|days| days.is_finite() && *days >= 0.0)
        .unwrap_or(wheel_targets::DEFAULT_IDLE_DAYS)
}

/// Render one target store. Pure so tests can inject a surveyed store instead
/// of depending on this host's real one.
pub fn render_wheel_targets(measured: &StoreSurvey, idle_days: f64) -> String {
    let store = measured.store().display_slash();
    if !measured.present() {
        return format!("wheel-build cargo targets under {store}: none (not created yet)\n");
    }
    let mut text = format!("wheel-build cargo targets under {store}:\n");
    if measured.targets().is_empty() {
        text.push_str("  none\n");
        return text;
    }
    for target in measured.targets() {
        let owner = target.owner().unwrap_or("unstamped");
        let verdict = match target.verdict() {
            Some(verdict) => format!("DEAD ({})", verdict.reason(idle_days)),
            None => "keep".to_string(),
        };
        text.push_str(&format!(
            "  {:<24} {:>9}  {:<34} {}\n",
            target.name(),
            format_bytes(target.bytes()),
            owner,
            verdict,
        ));
    }
    text.push_str(&format!(
        "  {:<24} {:>9}  {} reclaimable in {} dead target(s)\n",
        "total",
        format_bytes(measured.total_bytes()),
        format_bytes(measured.reclaimable_bytes()),
        measured.dead().count(),
    ));
    text
}

/// Render what a reclaim pass actually removed.
pub fn render_reclaimed(reclaimed: &[ReclaimedTarget]) -> String {
    if reclaimed.is_empty() {
        return "reclaimed: nothing\n".to_string();
    }
    let freed: u64 = reclaimed.iter().map(ReclaimedTarget::bytes_freed).sum();
    let mut text = format!(
        "reclaimed {} from {} dead target(s):\n",
        format_bytes(freed),
        reclaimed.len()
    );
    for target in reclaimed {
        text.push_str(&format!(
            "  - {} ({}, {})\n",
            target.name(),
            target.reason(),
            format_bytes(target.bytes_freed()),
        ));
    }
    text
}

/// Render the report. Pure so tests can inject volumes instead of depending on
/// the host's real disks.
pub fn render_disk_check(
    report: &PreflightReport,
    reclaimable: &[ReclaimableCache],
    cache_root: &Path,
    disabled: bool,
) -> DiskCheck {
    if disabled {
        return DiskCheck {
            text: format!(
                "disk check skipped: {MIN_DISK_ENV}=0 disables the pre-flight requirement\n"
            ),
            ok: true,
        };
    }

    let mut text = String::from("disk roots a cold ESP32 build writes:\n");
    for root in report.roots() {
        let verdict = if root.is_short() {
            format!("SHORT by {}", format_bytes(root.shortfall()))
        } else {
            format!(
                "ok ({} margin)",
                format_bytes(root.free_bytes().saturating_sub(root.required_bytes()))
            )
        };
        text.push_str(&format!(
            "  {:<8} {}  {} free, {} required  {}\n",
            root.label(),
            root.path().display(),
            format_bytes(root.free_bytes()),
            format_bytes(root.required_bytes()),
            verdict,
        ));
    }
    for (label, reason) in report.unchecked() {
        // A volume fbuild cannot measure is reported, never failed: the build
        // would warn the same way (`disk_preflight::preflight`).
        text.push_str(&format!(
            "  {label:<8} could not be measured ({reason}) — not counted as a failure\n"
        ));
    }

    text.push_str(&format!("reclaimable under {}:\n", cache_root.display()));
    if reclaimable.is_empty() {
        text.push_str("  none\n");
    } else {
        for cache in reclaimable {
            text.push_str(&format!(
                "  {:<24} {}\n",
                cache.name,
                format_bytes(cache.bytes)
            ));
        }
        let total: u64 = reclaimable.iter().map(|cache| cache.bytes).sum();
        text.push_str(&format!("  {:<24} {}\n", "total", format_bytes(total)));
    }

    if report.roots().is_empty() {
        // Every root failed to measure: say so instead of claiming success.
        text.push_str("verdict: unknown — no root could be measured\n");
        return DiskCheck { text, ok: true };
    }
    if report.is_sufficient() {
        text.push_str("verdict: ok — every measured root has room\n");
        return DiskCheck { text, ok: true };
    }
    text.push_str("verdict: FAIL — a cold ESP32 build would not fit\n");
    text.push_str(&report.failure_message(reclaimable, cache_root));
    DiskCheck { text, ok: false }
}

/// The project root to measure: the argument, or the current directory.
/// Lexical only (`std::path::absolute`) — `doctor` must not resolve symlinks
/// or touch the filesystem to guess what the project is.
fn project_dir_to_measure(raw: Option<&str>) -> NormalizedPath {
    let raw = raw.unwrap_or(".");
    match std::path::absolute(raw) {
        Ok(absolute) => NormalizedPath::from(absolute),
        Err(_) => NormalizedPath::from(Path::new(raw)),
    }
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
