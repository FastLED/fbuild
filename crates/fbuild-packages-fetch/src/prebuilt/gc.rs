//! Staging/partial garbage collection for prebuilt trees (#1882).
//!
//! [`extract`](super::extract) builds a tree in a sibling staging directory
//! named `.<first 16 hex of the sha256>.<attempt>.staging` and removes it on
//! every ordinary error path. A process that is killed outright (SIGKILL, the
//! OOM killer, a reboot) cannot run that cleanup, so the directory survives
//! holding up to the decompressed size ceiling — one abandoned attempt per kill.
//! This cell reaps exactly those directories and nothing else:
//!
//! - only names of the exact staging shape inside the given parent are
//!   candidates; a destination tree, a completion marker, a duplicate fragment
//!   or any other name is never touched;
//! - only real directories are removed: a symlink is never followed and never
//!   unlinked, so a candidate name cannot be used to delete a tree elsewhere;
//! - a candidate whose modification time is younger than the grace period (or
//!   in the future) is kept, so an extraction still running in another process
//!   is never damaged;
//! - a candidate carrying the completion marker is kept and reported: the tree
//!   verified but was never committed, and discarding a verified tree is the
//!   caller's decision, not a sweep's;
//! - one sweep removes at most [`MAX_REMOVALS`] directories, so the work is
//!   bounded on a cache that accumulated many abandoned attempts, and every
//!   removal error is reported instead of hidden.
//!
//! The disk-cache LRU and the bucket-key ownership are later cells of #1882.
use super::MARKER;
use fbuild_core::path::NormalizedPath;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Grace period a staging directory must exceed before a sweep may remove it.
pub const MIN_AGE: Duration = Duration::from_secs(60 * 60);
/// Most staging directories one sweep removes, so a call's work stays bounded.
pub const MAX_REMOVALS: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum GcError {
    #[error("{path}: {error}")]
    Io { path: String, error: String },
    #[error("staging sweep task failed: {0}")]
    Join(String),
}

/// What one sweep did, and what it deliberately left alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Staging directories removed by this sweep.
    pub removed: usize,
    /// Bytes those directories held, as measured before removal.
    pub freed_bytes: u64,
    /// Candidates younger than the grace period (an extraction may be live).
    pub kept_live: usize,
    /// Candidates holding the completion marker; a verified uncommitted tree.
    pub kept_marked: usize,
}

/// Remove abandoned staging directories directly inside `parent`, the directory
/// that holds the extracted trees this process created.
pub fn sweep_staging(parent: &Path, min_age: Duration) -> Result<SweepReport, GcError> {
    sweep_staging_at(parent, min_age, SystemTime::now())
}

/// [`sweep_staging`] on its own thread, for callers on the daemon runtime.
pub async fn sweep_staging_async(
    parent: NormalizedPath,
    min_age: Duration,
) -> Result<SweepReport, GcError> {
    tokio::task::spawn_blocking(move || sweep_staging(parent.as_path(), min_age))
        .await
        .map_err(|error| GcError::Join(error.to_string()))?
}

/// [`sweep_staging`] with an explicit clock, so the grace period is testable
/// without touching file times.
fn sweep_staging_at(
    parent: &Path,
    min_age: Duration,
    now: SystemTime,
) -> Result<SweepReport, GcError> {
    let mut report = SweepReport::default();
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        // Nothing was ever extracted here: an empty sweep, not an error.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(io_error(parent, error)),
    };
    for entry in entries {
        if report.removed >= MAX_REMOVALS {
            break;
        }
        let entry = entry.map_err(|error| io_error(parent, error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_staging_name(&name) {
            continue;
        }
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(&path, error)),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        match metadata.modified() {
            Ok(modified) => {
                // A modification time in the future yields the zero age, which
                // keeps the candidate: guessing "old" is the unsafe direction.
                if now.duration_since(modified).unwrap_or_default() < min_age {
                    report.kept_live += 1;
                    continue;
                }
            }
            Err(_) => {
                report.kept_live += 1;
                continue;
            }
        }
        let marker = path.join(MARKER);
        if std::fs::symlink_metadata(&marker).is_ok() {
            report.kept_marked += 1;
            continue;
        }
        let freed = tree_bytes(&path);
        std::fs::remove_dir_all(&path).map_err(|error| io_error(&path, error))?;
        report.removed += 1;
        report.freed_bytes = report.freed_bytes.saturating_add(freed);
    }
    Ok(report)
}

/// The exact staging name [`super::extract`] creates: a dot, the first 16 hex
/// digits of the archive digest, a decimal attempt, then the staging suffix.
fn is_staging_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some(rest) = rest.strip_suffix(".staging") else {
        return false;
    };
    let Some((digest, attempt)) = rest.split_once('.') else {
        return false;
    };
    digest.len() == 16
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && !attempt.is_empty()
        && attempt.bytes().all(|byte| byte.is_ascii_digit())
}

/// Sum the regular files in an abandoned tree, not following links.
fn tree_bytes(root: &Path) -> u64 {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .fold(0_u64, |sum, metadata| sum.saturating_add(metadata.len()))
}

fn io_error(path: &Path, error: std::io::Error) -> GcError {
    GcError::Io {
        path: path.display().to_string(),
        error: error.to_string(),
    }
}

#[cfg(test)]
#[path = "gc_tests.rs"]
mod tests;
