//! Pre-flight disk-space check for the roots a build will write
//! (FastLED/fbuild#1750).
//!
//! fbuild could measure free space (`fbuild_core::platform::fs::volume_facts`)
//! but no build path consulted it, so a full disk surfaced as a raw `ENOSPC`
//! from whichever subprocess happened to write next. [`preflight`] measures
//! every root *before* package resolution and turns a shortfall into one
//! actionable error: the path, its free and required bytes, the shortfall, and
//! the largest reclaimable caches under the cache root.
//!
//! This module only measures and reports. Reclaiming caches on demand (#1750
//! item 3) deliberately does not happen here — #1341 showed an eviction can
//! delete a warm toolchain mid-build, so reclaim stays an explicit, budgeted
//! step rather than a build side effect.

use std::io;
use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};

use crate::build_output::format_bytes;

/// One directory a build will write, and how many bytes it needs free there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskRoot {
    label: String,
    path: NormalizedPath,
    required_bytes: u64,
}

impl DiskRoot {
    /// A root the build writes, `label` naming it in the message, with the
    /// bytes it needs free. A `required_bytes` of zero skips the root.
    pub fn new(
        label: impl Into<String>,
        path: impl Into<NormalizedPath>,
        required_bytes: u64,
    ) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
            required_bytes,
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn required_bytes(&self) -> u64 {
        self.required_bytes
    }

    pub fn is_checked(&self) -> bool {
        self.required_bytes > 0
    }
}

/// Free space measured for one [`DiskRoot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSpace {
    label: String,
    path: NormalizedPath,
    free_bytes: u64,
    required_bytes: u64,
}

impl RootSpace {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn free_bytes(&self) -> u64 {
        self.free_bytes
    }

    pub fn required_bytes(&self) -> u64 {
        self.required_bytes
    }

    /// Bytes still missing before this root's requirement is met.
    pub fn shortfall(&self) -> u64 {
        self.required_bytes.saturating_sub(self.free_bytes)
    }

    pub fn is_short(&self) -> bool {
        self.shortfall() > 0
    }
}

/// A depth-one directory under the cache root, sized for the failure message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimableCache {
    pub name: String,
    pub bytes: u64,
}

/// Where free space is measured. A trait so tests can inject a volume instead
/// of depending on the host's real disks.
pub trait FreeSpace {
    fn available(&self, path: &Path) -> io::Result<u64>;
}

/// The host's real volumes.
pub struct HostFreeSpace;

impl FreeSpace for HostFreeSpace {
    fn available(&self, path: &Path) -> io::Result<u64> {
        fbuild_core::platform::fs::volume_facts(path).map(|facts| facts.available_space)
    }
}

/// Measurement for every checked root, plus the roots that could not be
/// measured (reported as warnings, never as failures — an unreadable volume
/// must not block a build that might otherwise work).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreflightReport {
    roots: Vec<RootSpace>,
    unchecked: Vec<(String, String)>,
}

impl PreflightReport {
    pub fn roots(&self) -> &[RootSpace] {
        &self.roots
    }

    pub fn unchecked(&self) -> &[(String, String)] {
        &self.unchecked
    }

    pub fn short_roots(&self) -> impl Iterator<Item = &RootSpace> {
        self.roots.iter().filter(|root| root.is_short())
    }

    pub fn is_sufficient(&self) -> bool {
        self.short_roots().next().is_none()
    }

    /// The single line a healthy build prints (#1750 acceptance criterion 1).
    pub fn info_line(&self) -> String {
        if self.roots.is_empty() {
            return "disk preflight: skipped (no root declares a requirement)".to_string();
        }
        let parts: Vec<String> = self
            .roots
            .iter()
            .map(|root| {
                format!(
                    "{} {} free, {} required",
                    root.label,
                    format_bytes(root.free_bytes),
                    format_bytes(root.required_bytes)
                )
            })
            .collect();
        format!(
            "disk preflight: ok ({}; {})",
            parts.join(", "),
            self.shortest()
        )
    }

    fn shortest(&self) -> String {
        let tightest = self
            .roots
            .iter()
            .min_by_key(|root| root.free_bytes.saturating_sub(root.required_bytes));
        match tightest {
            Some(root) => format!(
                "{} is the tightest root with {} margin",
                root.label,
                format_bytes(root.free_bytes.saturating_sub(root.required_bytes))
            ),
            None => "no checked roots".to_string(),
        }
    }

    /// The actionable failure text (#1750 acceptance criterion 2): path, free
    /// vs required, the shortfall, and what could be reclaimed.
    pub fn failure_message(&self, reclaimable: &[ReclaimableCache], cache_root: &Path) -> String {
        let mut text = String::from("insufficient disk space:");
        for root in self.short_roots() {
            text.push_str(&format!(
                "\n  {} {} has {} free, needs {}, short by {}",
                root.label,
                root.path.display(),
                format_bytes(root.free_bytes),
                format_bytes(root.required_bytes),
                format_bytes(root.shortfall())
            ));
        }
        let reclaimable_total: u64 = reclaimable.iter().map(|cache| cache.bytes).sum();
        text.push_str(&reclaimable_line(reclaimable, cache_root));
        if reclaimable_total < self.total_shortfall() {
            text.push_str(&format!(
                "\n  reclaiming every cache frees {} — the rest must come from this volume",
                format_bytes(reclaimable_total)
            ));
        }
        text.push('\n');
        text.push_str(fbuild_core::disk_full::DISK_FULL_HINT);
        text
    }

    pub fn total_shortfall(&self) -> u64 {
        self.short_roots().map(RootSpace::shortfall).sum()
    }
}

/// What a user could free without touching their project, as one message line.
///
/// Shared by the pre-flight failure (#1750 acceptance criterion 2) and the
/// translated mid-build `ENOSPC` ([`enospc_failure`]) so both list the same
/// caches in the same order.
fn reclaimable_line(reclaimable: &[ReclaimableCache], cache_root: &Path) -> String {
    if reclaimable.is_empty() {
        return format!("\n  no reclaimable caches under {}", cache_root.display());
    }
    let total: u64 = reclaimable.iter().map(|cache| cache.bytes).sum();
    let listed: Vec<String> = reclaimable
        .iter()
        .map(|cache| format!("{} {}", cache.name, format_bytes(cache.bytes)))
        .collect();
    format!(
        "\n  reclaimable under {}: {} ({} total)",
        cache_root.display(),
        listed.join(", "),
        format_bytes(total)
    )
}

/// The pre-flight message, for an `ENOSPC` a tool printed *after* the check
/// passed (FastLED/fbuild#1750 item 4).
///
/// Returns `None` when `tool_output` shows no sign of a full volume, so callers
/// fall through to their own error text — a link failure from an undefined
/// reference must not be re-labelled a disk failure.
pub fn enospc_failure(context: &str, tool_output: &str, cache_root: &Path) -> Option<String> {
    let signature = fbuild_core::disk_full::enospc_signature(tool_output)?;
    let reclaimable = reclaimable_caches(cache_root);
    let mut text = format!(
        "insufficient disk space:\n  \
         {context} failed: the volume is full ({signature})\n  \
         the pre-flight check passed, so the volume filled up during the build"
    );
    text.push_str(&reclaimable_line(&reclaimable, cache_root));
    text.push('\n');
    text.push_str(fbuild_core::disk_full::DISK_FULL_HINT);
    // #1750 item 6: give the failure path room on the volume that just filled.
    text.push_str(&fbuild_core::disk_reserve::release_note(context));
    Some(text)
}

/// Free space on the volume `path` lives on, walking up to the nearest
/// existing ancestor when `path` itself does not exist yet.
///
/// A build root that the *first* build creates (`<cache>/tmp`) still lives on
/// some volume, and reporting "could not be measured" for it is a false warning
/// on every fresh host — `fbuild doctor --check-disk` showed exactly that. Only
/// `NotFound` walks up; a permission or I/O error stays a root fbuild cannot
/// measure, so a genuinely unreadable volume is still reported as such.
pub(crate) fn available_on_volume(probe: &dyn FreeSpace, path: &Path) -> io::Result<u64> {
    let mut current = path;
    loop {
        match probe.available(current) {
            Ok(free) => return Ok(free),
            Err(error) if error.kind() == io::ErrorKind::NotFound => match current.parent() {
                Some(parent) if parent != current => current = parent,
                _ => return Err(error),
            },
            Err(error) => return Err(error),
        }
    }
}

/// Measure every checked root without judging it.
pub fn measure(probe: &dyn FreeSpace, roots: &[DiskRoot]) -> PreflightReport {
    let mut report = PreflightReport::default();
    for root in roots.iter().filter(|root| root.is_checked()) {
        match available_on_volume(probe, root.path.as_ref()) {
            Ok(free_bytes) => report.roots.push(RootSpace {
                label: root.label.clone(),
                path: root.path.clone(),
                free_bytes,
                required_bytes: root.required_bytes,
            }),
            Err(error) => report
                .unchecked
                .push((root.label.clone(), error.to_string())),
        }
    }
    report
}

/// Measure `roots` and fail with one actionable error when any root is short.
pub fn preflight(
    probe: &dyn FreeSpace,
    roots: &[DiskRoot],
    cache_root: &Path,
) -> Result<PreflightReport> {
    let report = measure(probe, roots);
    for (label, reason) in report.unchecked() {
        tracing::warn!(root = %label, %reason, "disk preflight could not measure a root");
    }
    if report.is_sufficient() {
        tracing::info!("{}", report.info_line());
        return Ok(report);
    }
    let reclaimable = reclaimable_caches(cache_root);
    Err(FbuildError::BuildFailed(
        report.failure_message(&reclaimable, cache_root),
    ))
}

/// [`enospc_failure`] as the `Err` a tool boundary returns, or `None` when the
/// output is not a full-volume failure.
pub fn enospc_error(context: &str, tool_output: &str, cache_root: &Path) -> Option<FbuildError> {
    enospc_failure(context, tool_output, cache_root).map(FbuildError::BuildFailed)
}

/// Depth-one cache directories, largest first — the bytes a user could free
/// without touching their project.
pub fn reclaimable_caches(cache_root: &Path) -> Vec<ReclaimableCache> {
    let Ok(entries) = std::fs::read_dir(cache_root) else {
        return Vec::new();
    };
    let mut caches: Vec<ReclaimableCache> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| ReclaimableCache {
            name: entry.file_name().to_string_lossy().into_owned(),
            bytes: directory_bytes(&entry.path()),
        })
        .filter(|cache| cache.bytes > 0)
        .collect();
    caches.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.name.cmp(&right.name))
    });
    caches
}

fn directory_bytes(root: &Path) -> u64 {
    walkdir::WalkDir::new(root)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}
