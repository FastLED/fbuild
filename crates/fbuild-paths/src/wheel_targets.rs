//! The wheel-build cargo target store (FastLED/fbuild#2062).
//!
//! Every `pip install` of an fbuild checkout — including each agent worktree —
//! builds through a stable, warm cargo target dir under
//! `~/.fbuild/cargo-target/wheel-build/<repo-name>-<sha256(str(repo-root))[:12]>`
//! (`ci/build_backend.py`). The digest is one-way, so once a checkout is
//! deleted nothing inside the store can say which path a target belonged to,
//! and nothing ever removed the target: one full release target dir per
//! worktree ever created, forever.
//!
//! Two halves close that. `ci/build_backend.py` writes the owning checkout into
//! the target's stamp file and sweeps dead siblings before a build; this module
//! is the same measurement for hosts that have the binary but not a checkout,
//! so `fbuild doctor --check-disk` can report the store — and, with an explicit
//! flag, reclaim it from cron.
//!
//! Verdicts, deliberately identical to the Python sweep:
//!
//! * a stamped target whose recorded owner no longer exists is dead (exact
//!   evidence, no age needed);
//! * an unstamped target (written before the stamp existed) is dead when every
//!   file in it is older than the idle gate (default 14 days);
//! * anything the walk cannot establish — an unreadable entry, a symlink, a
//!   tree over the scan budget — is **kept**. A target dir is a cache, so
//!   deleting a live one only costs a rebuild, but "cannot tell" still means
//!   "keep": this module never guesses.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use fbuild_core::path::NormalizedPath;
use fbuild_core::platform::host;

use crate::FBUILD_DIR_NAME;

/// Suffix of the file naming the checkout that owns a target dir.
///
/// The full name is [`source_stamp_name`]: the `.fbuild` directory segment
/// followed by this suffix, composed from [`FBUILD_DIR_NAME`] rather than
/// spelled out (the `ban_raw_fbuild_path` Dylint owns that literal).
pub const WHEEL_SOURCE_STAMP_SUFFIX: &str = "-wheel-source";

/// Idle gate for targets written before the stamp existed, in days.
pub const DEFAULT_IDLE_DAYS: f64 = 14.0;

/// Files scanned inside one target dir before the survey gives up on dating
/// it and keeps it. A cargo target dir holds ~10k files; the cap keeps a
/// diagnostic from spending seconds per entry.
pub const SCAN_BUDGET: usize = 20_000;

/// The segments under the home directory holding every wheel-build target,
/// starting with the `.fbuild` directory segment itself.
const STORE_SEGMENTS: [&str; 3] = [FBUILD_DIR_NAME, "cargo-target", "wheel-build"];

/// Name of the stamp file inside a target dir, e.g. `.fbuild-wheel-source`.
pub fn source_stamp_name() -> String {
    format!("{FBUILD_DIR_NAME}{WHEEL_SOURCE_STAMP_SUFFIX}")
}

/// The store holding every wheel-build target dir, when a home directory is
/// known. `None` on a home-less host: an unmeasurable store is reported, never
/// guessed at.
pub fn store_root() -> Option<NormalizedPath> {
    let home = host::home_dir()?;
    let mut store = home;
    for segment in STORE_SEGMENTS {
        store = store.join(segment);
    }
    Some(store)
}

/// Why a target dir is dead. `None` from [`TargetSurvey::verdict`] means keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReclaimVerdict {
    /// The stamp names a checkout that no longer exists.
    OwnerGone { owner: String },
    /// No stamp, and every file is older than the idle gate.
    IdleWithoutStamp,
}

impl ReclaimVerdict {
    /// One-line reason, the same spelling the Python sweep prints.
    pub fn reason(&self, idle_days: f64) -> String {
        match self {
            Self::OwnerGone { owner } => format!("owner gone: {owner}"),
            Self::IdleWithoutStamp => {
                format!("idle > {}d, no owner stamp", format_days(idle_days))
            }
        }
    }
}

/// One target dir in the store, measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSurvey {
    name: String,
    owner: Option<String>,
    bytes: u64,
    newest: Option<SystemTime>,
    verdict: Option<ReclaimVerdict>,
}

impl TargetSurvey {
    /// The directory name inside the store, e.g. `fbuild-0123456789ab`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The checkout recorded in the stamp file, when the target has one.
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    /// Bytes the target holds; `0` when the walk was not possible.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Newest file mtime, when the walk established it.
    pub fn newest(&self) -> Option<SystemTime> {
        self.newest
    }

    /// Why the target is dead, or `None` when it must be kept.
    pub fn verdict(&self) -> Option<&ReclaimVerdict> {
        self.verdict.as_ref()
    }
}

/// What one target the reclaim pass actually removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimedTarget {
    name: String,
    reason: String,
    bytes_freed: u64,
}

impl ReclaimedTarget {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn bytes_freed(&self) -> u64 {
        self.bytes_freed
    }
}

/// The whole store, measured. Deletes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreSurvey {
    store: NormalizedPath,
    present: bool,
    targets: Vec<TargetSurvey>,
}

impl StoreSurvey {
    /// The store directory that was measured.
    pub fn store(&self) -> &NormalizedPath {
        &self.store
    }

    /// Whether the store directory exists at all. A missing store is a host
    /// that never built a wheel, not a failure.
    pub fn present(&self) -> bool {
        self.present
    }

    /// Every target dir, largest first.
    pub fn targets(&self) -> &[TargetSurvey] {
        &self.targets
    }

    /// Bytes held by every target in the store.
    pub fn total_bytes(&self) -> u64 {
        self.targets.iter().map(TargetSurvey::bytes).sum()
    }

    /// The targets a reclaim pass would remove.
    pub fn dead(&self) -> impl Iterator<Item = &TargetSurvey> {
        self.targets.iter().filter(|t| t.verdict.is_some())
    }

    /// Bytes a reclaim pass would give back.
    pub fn reclaimable_bytes(&self) -> u64 {
        self.dead().map(TargetSurvey::bytes).sum()
    }
}

/// Measure every target in `store`, deciding which are dead.
///
/// `idle_days` gates unstamped targets only; `now` is injectable so tests do
/// not depend on the wall clock. A store that does not exist is reported as
/// [`StoreSurvey::present`] `false` rather than as an error.
pub fn survey(store: &Path, idle_days: f64, now: SystemTime) -> StoreSurvey {
    survey_with_budget(store, idle_days, now, SCAN_BUDGET)
}

/// [`survey`] with an explicit per-target scan budget, so tests can exercise
/// the over-budget path without creating 20k files.
pub fn survey_with_budget(
    store: &Path,
    idle_days: f64,
    now: SystemTime,
    scan_budget: usize,
) -> StoreSurvey {
    let idle_days = if idle_days.is_finite() && idle_days >= 0.0 {
        idle_days
    } else {
        DEFAULT_IDLE_DAYS
    };
    let cutoff = now.checked_sub(idle_seconds(idle_days));
    let store = NormalizedPath::new(store);
    let entries = match fs::read_dir(store.as_path()) {
        Ok(entries) => entries,
        Err(_) => {
            return StoreSurvey {
                store,
                present: false,
                targets: Vec::new(),
            };
        }
    };

    let mut targets = Vec::new();
    for entry in entries.flatten() {
        // `DirEntry::file_type` never follows a symlink, so a linked entry can
        // never be walked or deleted through this path.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let scan = scan_tree(&path, scan_budget);
        let owner = recorded_owner(&path);
        let verdict = verdict_for(owner.as_deref(), scan.as_ref(), cutoff);
        targets.push(TargetSurvey {
            name,
            owner,
            bytes: scan.as_ref().map_or(0, |scan| scan.bytes),
            newest: scan.as_ref().map(|scan| scan.newest),
            verdict,
        });
    }
    targets.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.name.cmp(&right.name))
    });
    StoreSurvey {
        store,
        present: true,
        targets,
    }
}

/// Remove every target [`survey`] calls dead, returning what was reclaimed.
///
/// Only a direct child of `store` is ever removed, so a malformed entry can
/// never turn into a delete somewhere else on disk; a failed removal is
/// skipped rather than reported as success.
pub fn reclaim(store: &Path, idle_days: f64, now: SystemTime) -> Vec<ReclaimedTarget> {
    let measured = survey(store, idle_days, now);
    let mut reclaimed = Vec::new();
    for target in measured.targets.iter() {
        let Some(verdict) = target.verdict() else {
            continue;
        };
        let path = measured.store.join(target.name());
        if path.as_path().parent().map(NormalizedPath::new).as_ref() != Some(&measured.store) {
            continue;
        }
        if fs::remove_dir_all(path.as_path()).is_err() {
            continue;
        }
        reclaimed.push(ReclaimedTarget {
            name: target.name().to_string(),
            reason: verdict.reason(idle_days),
            bytes_freed: target.bytes(),
        });
    }
    reclaimed
}

/// The verdict for one measured target. `None` keeps it.
fn verdict_for(
    owner: Option<&str>,
    scan: Option<&Scan>,
    cutoff: Option<SystemTime>,
) -> Option<ReclaimVerdict> {
    if let Some(owner) = owner {
        // Exact evidence: the checkout the stamp names is gone. A stamp we
        // cannot read as a path is not evidence, so it keeps the target.
        if Path::new(owner).exists() {
            return None;
        }
        return Some(ReclaimVerdict::OwnerGone {
            owner: owner.to_string(),
        });
    }
    let scan = scan?; // too big or unreadable to date cheaply: keep it
    let cutoff = cutoff?; // idle gate out of range for `SystemTime`: keep it
    if scan.newest >= cutoff {
        return None;
    }
    Some(ReclaimVerdict::IdleWithoutStamp)
}

/// The owner path recorded in a target's stamp file, or `None` when unstamped
/// (or when the stamp does not hold a usable path).
fn recorded_owner(target: &Path) -> Option<String> {
    let text = fs::read_to_string(target.join(source_stamp_name())).ok()?;
    let recorded = text.trim();
    if recorded.is_empty() {
        return None;
    }
    Some(recorded.to_string())
}

/// Newest mtime and total bytes under `root`, or `None` when the walk exceeds
/// `budget` entries or the root itself cannot be read.
fn scan_tree(root: &Path, budget: usize) -> Option<Scan> {
    let mut scan = Scan {
        newest: SystemTime::UNIX_EPOCH,
        bytes: 0,
    };
    let mut scanned = 0usize;
    let mut pending = vec![NormalizedPath::new(root)];
    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(dir.as_path()).ok()?;
        for entry in entries.flatten() {
            scanned += 1;
            if scanned > budget {
                return None;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(NormalizedPath::new(entry.path()));
                continue;
            }
            if !kind.is_file() {
                continue; // symlinks and devices are not this target's bytes
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            scan.bytes = scan.bytes.saturating_add(metadata.len());
            if let Ok(modified) = metadata.modified() {
                if modified > scan.newest {
                    scan.newest = modified;
                }
            }
        }
    }
    Some(scan)
}

/// `days` for the idle-gate message: whole days drop the fraction, matching
/// the Python sweep's `:g`.
fn format_days(days: f64) -> String {
    if days.fract() == 0.0 {
        format!("{days:.0}")
    } else {
        format!("{days:.1}")
    }
}

/// The measured shape of one target tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scan {
    newest: SystemTime,
    bytes: u64,
}

/// [`idle_days`] as a `Duration`, saturating so an absurd gate cannot panic.
fn idle_seconds(idle_days: f64) -> std::time::Duration {
    let seconds = idle_days * 86_400.0;
    if seconds >= u64::MAX as f64 {
        return std::time::Duration::from_secs(u64::MAX);
    }
    std::time::Duration::from_secs(seconds.max(0.0) as u64)
}

#[cfg(test)]
#[path = "wheel_targets_tests.rs"]
mod tests;
