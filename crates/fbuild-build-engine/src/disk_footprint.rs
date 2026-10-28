//! Per-stage disk footprint measurement for a build (FastLED/fbuild#1750 item 1).
//!
//! [`crate::disk_preflight`] judges a *budget* before a build starts. This module
//! does the opposite: it measures what a build actually consumes, stage by
//! stage, so those budgets can be replaced by observation instead of estimates.
//! Sampling free space at each stage boundary gives the peak delta per root —
//! the number a minimum-disk requirement has to cover — without walking the
//! cache (a walk of a warm cache takes ~60 s and the daemon's GC can move it
//! mid-build).
//!
//! ## Enabling
//!
//! Off by default, with the same shape as `crate::perf_log`:
//!
//! - `FBUILD_DISK_FOOTPRINT=1` emits one summary line per build.
//! - `FBUILD_DISK_FOOTPRINT_JSON=<absolute path>` also appends one JSON line
//!   per build and implies the flag, so measurements accumulate for whoever
//!   tightens the constants.
//!
//! Read inside the daemon, which is where builds run: `fbuild build` reuses a
//! running daemon and does not forward environment variables, so the value has
//! to be in the environment that spawned the daemon (same convention as
//! `FBUILD_PERF_LOG` and `FBUILD_MIN_DISK_BYTES`).
//!
//! ## What is measured
//!
//! Free space, not directory size: a delta between two samples is exactly the
//! bytes the build added to (or freed from) that volume, including everything
//! written by subprocesses and by the embedded zccache service — which a
//! directory walk of the project tree would miss.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

use fbuild_core::path::NormalizedPath;

use crate::build_output::format_bytes;
use crate::disk_preflight::{FreeSpace, available_on_volume};

/// Enables the per-build footprint summary.
pub const DISK_FOOTPRINT_ENV: &str = "FBUILD_DISK_FOOTPRINT";

/// Appends one machine-readable JSON line per build to this path. Implies
/// [`DISK_FOOTPRINT_ENV`].
pub const DISK_FOOTPRINT_JSON_ENV: &str = "FBUILD_DISK_FOOTPRINT_JSON";

/// Resolved once per process: the environment of the daemon that runs builds.
struct Config {
    enabled: bool,
    sink: Option<OsString>,
}

fn config() -> &'static Config {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    CONFIG.get_or_init(|| {
        // Keep the raw value: lexical normalization could resolve `..` across
        // symlinks and open a different file than the caller named.
        let sink = std::env::var_os(DISK_FOOTPRINT_JSON_ENV).filter(|value| !value.is_empty());
        let flagged = std::env::var(DISK_FOOTPRINT_ENV)
            .map(|value| !value.is_empty() && value != "0")
            .unwrap_or(false);
        Config {
            enabled: flagged || sink.is_some(),
            sink,
        }
    })
}

/// Whether footprint measurement is on for this process.
pub fn enabled() -> bool {
    config().enabled
}

/// The `FBUILD_DISK_FOOTPRINT_JSON` sink path, when set and non-empty.
pub fn json_sink_path() -> Option<&'static Path> {
    config().sink.as_ref().map(Path::new)
}

/// One directory a footprint measurement watches.
///
/// No requirement is attached: this module reports what happened, it never
/// decides whether a build may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FootprintRoot {
    label: String,
    path: NormalizedPath,
}

impl FootprintRoot {
    pub fn new(label: impl Into<String>, path: impl Into<NormalizedPath>) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// One root's measured delta for one stage: positive means the build consumed
/// bytes, negative means it freed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootDelta {
    label: String,
    delta: i64,
}

impl RootDelta {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn delta(&self) -> i64 {
        self.delta
    }
}

/// The deltas measured between two stage boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageFootprint {
    stage: &'static str,
    deltas: Vec<RootDelta>,
}

impl StageFootprint {
    pub fn stage(&self) -> &str {
        self.stage
    }

    pub fn deltas(&self) -> &[RootDelta] {
        &self.deltas
    }

    /// This stage's delta for `label`, when that root was measurable.
    pub fn delta_for(&self, label: &str) -> Option<i64> {
        self.deltas
            .iter()
            .find(|delta| delta.label == label)
            .map(|delta| delta.delta)
    }
}

/// Samples free space per root at stage boundaries and reports the peak.
///
/// Cheap no-op when the feature is off: [`Self::sample`] returns immediately and
/// nothing is printed on drop.
pub struct DiskFootprint<'a, P: FreeSpace> {
    label: &'static str,
    probe: &'a P,
    roots: Vec<FootprintRoot>,
    /// Free bytes when the recorder was created, `None` when unmeasurable.
    start: Vec<Option<u64>>,
    /// Free bytes at the previous sample.
    last: Vec<Option<u64>>,
    /// Lowest free-byte reading seen so far, for the peak consumption.
    lowest: Vec<Option<u64>>,
    stages: Vec<StageFootprint>,
    active: bool,
}

impl<'a, P: FreeSpace> DiskFootprint<'a, P> {
    /// Start measuring `roots` for `label` (`"esp32"`, `"avr"`, …).
    pub fn new(label: &'static str, probe: &'a P, roots: Vec<FootprintRoot>) -> Self {
        Self::with_active(label, probe, roots, enabled())
    }

    /// Test seam: the same recorder with the flag stated outright.
    #[cfg(test)]
    pub(crate) fn for_test(
        label: &'static str,
        probe: &'a P,
        roots: Vec<FootprintRoot>,
        active: bool,
    ) -> Self {
        Self::with_active(label, probe, roots, active)
    }

    fn with_active(
        label: &'static str,
        probe: &'a P,
        roots: Vec<FootprintRoot>,
        active: bool,
    ) -> Self {
        let mut recorder = Self {
            label,
            probe,
            roots,
            start: Vec::new(),
            last: Vec::new(),
            lowest: Vec::new(),
            stages: Vec::new(),
            active,
        };
        if active {
            let readings = recorder.read_all();
            recorder.start = readings.clone();
            recorder.lowest = readings.clone();
            recorder.last = readings;
        }
        recorder
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The stages sampled so far, in build order.
    pub fn stages(&self) -> &[StageFootprint] {
        &self.stages
    }

    /// The largest consumption measured for `label` across all samples: the
    /// peak delta a minimum-disk requirement for this build has to cover.
    ///
    /// `None` when that root was never measurable.
    pub fn peak_consumed(&self, label: &str) -> Option<u64> {
        let index = self.roots.iter().position(|root| root.label == label)?;
        let start = self.start.get(index).copied().flatten()?;
        let lowest = self.lowest.get(index).copied().flatten()?;
        Some(start.saturating_sub(lowest))
    }

    /// Close the current stage and record its deltas.
    ///
    /// Unmeasurable roots are skipped (and warned about once), never fatal: a
    /// footprint log must not be able to fail a build.
    pub fn sample(&mut self, stage: &'static str) {
        if !self.active {
            return;
        }
        let readings = self.read_all();
        let mut deltas = Vec::new();
        for (index, root) in self.roots.iter().enumerate() {
            let (Some(last), Some(now)) = (self.last[index], readings[index]) else {
                continue;
            };
            // `now > last` means the volume gained space (a GC ran, or a sibling
            // process finished): the delta is negative, the peak never is.
            deltas.push(RootDelta {
                label: root.label.clone(),
                delta: last as i64 - now as i64,
            });
            self.lowest[index] = Some(self.lowest[index].map_or(now, |low| low.min(now)));
        }
        self.last = readings;
        self.stages.push(StageFootprint { stage, deltas });
    }

    /// Free space for every root now, `None` where the volume cannot be read.
    fn read_all(&self) -> Vec<Option<u64>> {
        self.roots
            .iter()
            .map(
                |root| match available_on_volume(self.probe, root.path.as_ref()) {
                    Ok(free) => Some(free),
                    Err(error) => {
                        tracing::warn!(
                            root = root.label.as_str(),
                            path = %root.path.display(),
                            %error,
                            "disk footprint could not measure a root"
                        );
                        None
                    }
                },
            )
            .collect()
    }

    /// The summary as one line (no newlines), for the build log.
    pub fn summary_line(&self) -> String {
        let stages: Vec<String> = self
            .stages
            .iter()
            .map(|stage| format!("{}: {}", stage.stage, deltas_text(stage.deltas())))
            .collect();
        let peaks: Vec<String> = self
            .roots
            .iter()
            .filter_map(|root| {
                self.peak_consumed(&root.label)
                    .map(|peak| format!("{} {}", root.label, format_bytes(peak)))
            })
            .collect();
        let stages = if stages.is_empty() {
            "no stage sampled".to_string()
        } else {
            stages.join(" | ")
        };
        let peaks = if peaks.is_empty() {
            "unmeasurable".to_string()
        } else {
            peaks.join(", ")
        };
        format!(
            "[disk-footprint {}] {} | peak consumed {}",
            self.label, stages, peaks
        )
    }

    /// The same data as one JSON object, for accumulating measurements.
    pub fn summary_json(&self) -> serde_json::Value {
        let stages: serde_json::Map<String, serde_json::Value> = self
            .stages
            .iter()
            .map(|stage| {
                let deltas: serde_json::Map<String, serde_json::Value> = stage
                    .deltas()
                    .iter()
                    .map(|delta| (delta.label.clone(), serde_json::json!(delta.delta)))
                    .collect();
                (stage.stage.to_string(), serde_json::Value::Object(deltas))
            })
            .collect();
        let mut peaks = serde_json::Map::new();
        let mut start_free = serde_json::Map::new();
        for (index, root) in self.roots.iter().enumerate() {
            if let Some(peak) = self.peak_consumed(&root.label) {
                peaks.insert(root.label.clone(), serde_json::json!(peak));
            }
            if let Some(free) = self.start.get(index).copied().flatten() {
                start_free.insert(root.label.clone(), serde_json::json!(free));
            }
        }
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or(0);
        serde_json::json!({
            "label": self.label,
            "stages": stages,
            "peak_bytes": peaks,
            "start_free_bytes": start_free,
            "unix_ms": unix_ms,
        })
    }

    /// Append this build's line to `path` (create + append).
    pub fn write_json(&self, path: &Path) -> std::io::Result<()> {
        let mut line = serde_json::to_string(&self.summary_json())?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?;
        file.write_all(line.as_bytes())
    }
}

impl<P: FreeSpace> Drop for DiskFootprint<'_, P> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let line = self.summary_line();
        // Mirror to tracing and stderr, exactly like the perf log, so the line
        // is visible whether or not a subscriber is attached.
        tracing::info!(target: "fbuild_build::disk_footprint", "{}", line);
        eprintln!("{}", line);
        if let Some(path) = json_sink_path() {
            if let Err(error) = self.write_json(path) {
                tracing::warn!(
                    target: "fbuild_build::disk_footprint",
                    "failed to append footprint JSON to {}: {}",
                    path.display(),
                    error
                );
            }
        }
    }
}

/// `cache +1.20GB, project -512.00MB` for one stage.
fn deltas_text(deltas: &[RootDelta]) -> String {
    if deltas.is_empty() {
        return "not measured".to_string();
    }
    deltas
        .iter()
        .map(|delta| format!("{} {}", delta.label, delta_text(delta.delta)))
        .collect::<Vec<String>>()
        .join(", ")
}

fn delta_text(delta: i64) -> String {
    match delta.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{}", format_bytes(delta.unsigned_abs())),
        std::cmp::Ordering::Less => format!("-{}", format_bytes(delta.unsigned_abs())),
        std::cmp::Ordering::Equal => "unchanged".to_string(),
    }
}
