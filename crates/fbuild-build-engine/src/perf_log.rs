//! Env-gated phase timing for warm-build investigation (FastLED/fbuild#91).
//!
//! Collects per-phase wall-clock measurements across a single `BuildOrchestrator`
//! invocation and emits a compact summary on drop. The feature is off by default
//! and adds only a handful of cheap `Instant::now()` calls to the hot path.
//!
//! ## Enabling
//!
//! Set `FBUILD_PERF_LOG=1` (on either the CLI caller or the daemon process —
//! the summary is emitted by whichever side owns the timer). The summary is
//! written via `tracing::info!` under the `fbuild_build::perf_log` target and
//! also mirrored to stderr so it is visible in CLI output without requiring a
//! tracing subscriber reconfiguration.
//!
//! Set `FBUILD_PERF_LOG_JSON=<absolute file path>` to additionally append one
//! machine-readable JSON line per timer on drop (FastLED/fbuild#1465):
//! `{"label":..,"phases":{"<name>":<ms f64>,..},"total_ms":<f64>,"unix_ms":<u64>}`.
//! Setting it also enables the timer, exactly like `FBUILD_PERF_LOG=1`.
//!
//! ## Usage
//!
//! ```ignore
//! use crate::perf_log::PerfTimer;
//! let mut perf = PerfTimer::new("warm-pass");
//! {
//!     let _g = perf.phase("config-parse");
//!     // ...
//! }
//! // auto-summary on drop
//! ```

use fbuild_core::path::NormalizedPath;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Returns `true` when `FBUILD_PERF_LOG=1` (or any non-empty, non-`0` value)
/// or when `FBUILD_PERF_LOG_JSON` names a sink file.
///
/// Cached after the first call so repeated checks are O(1).
pub fn enabled() -> bool {
    static CACHED: AtomicBool = AtomicBool::new(false);
    static INIT: AtomicBool = AtomicBool::new(false);
    if !INIT.load(Ordering::Relaxed) {
        let v = std::env::var("FBUILD_PERF_LOG")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
            || json_sink_path().is_some();
        CACHED.store(v, Ordering::Relaxed);
        INIT.store(true, Ordering::Relaxed);
    }
    CACHED.load(Ordering::Relaxed)
}

/// Returns the `FBUILD_PERF_LOG_JSON` sink path when set and non-empty.
///
/// Cached after the first call.
pub fn json_sink_path() -> Option<&'static Path> {
    static SINK: OnceLock<Option<NormalizedPath>> = OnceLock::new();
    SINK.get_or_init(|| {
        std::env::var_os("FBUILD_PERF_LOG_JSON")
            .filter(|v| !v.is_empty())
            .map(NormalizedPath::new)
    })
    .as_ref()
    .map(NormalizedPath::as_path)
}

/// Append `value` as a single JSON line to `path` (append + create).
fn append_json_line(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    f.write_all(line.as_bytes())
}

fn duration_ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// A single phase's accumulated duration.
struct Phase {
    name: &'static str,
    total: Duration,
}

/// Collects phase durations and emits a summary on drop.
///
/// Cheap no-op when `FBUILD_PERF_LOG` is not set — all phase guards become
/// zero-work RAII objects.
pub struct PerfTimer {
    label: &'static str,
    start: Instant,
    phases: Vec<Phase>,
    active: bool,
}

impl PerfTimer {
    /// Create a timer rooted at `Instant::now()`. Auto-emits summary on drop.
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            start: Instant::now(),
            phases: Vec::new(),
            active: enabled(),
        }
    }

    /// Return whether this timer is actively emitting/recording diagnostics.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Emit an immediate wall-clock checkpoint without starting a timed phase.
    pub fn checkpoint(&self, name: impl AsRef<str>) {
        if !self.active {
            return;
        }
        self.emit_event("checkpoint", name.as_ref(), Duration::from_millis(0));
    }

    /// Start a phase; finishes when the returned guard drops.
    pub fn phase(&mut self, name: &'static str) -> PhaseGuard<'_> {
        if self.active {
            self.emit_event("phase-start", name, Duration::from_millis(0));
        }
        PhaseGuard {
            owner: self,
            name,
            start: Instant::now(),
        }
    }

    /// Add a manually-measured duration (e.g. when a phase is split across
    /// closures that can't share a `&mut PerfTimer`).
    pub fn record(&mut self, name: &'static str, dur: Duration) {
        if !self.active {
            return;
        }
        if let Some(p) = self.phases.iter_mut().find(|p| p.name == name) {
            p.total += dur;
        } else {
            self.phases.push(Phase { name, total: dur });
        }
    }

    /// Build the structured JSON summary line for this timer.
    fn summary_json(&self, total: Duration) -> serde_json::Value {
        let phases: serde_json::Map<String, serde_json::Value> = self
            .phases
            .iter()
            .map(|p| (p.name.to_string(), serde_json::json!(duration_ms(p.total))))
            .collect();
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        serde_json::json!({
            "label": self.label,
            "phases": phases,
            "total_ms": duration_ms(total),
            "unix_ms": unix_ms,
        })
    }

    fn emit_event(&self, event: &str, name: &str, duration: Duration) {
        let wall_ms = self.start.elapsed().as_millis();
        let phase_ms = duration.as_millis();
        let line = format!(
            "[perf-log {}] {} last_phase={} wall={} ms phase={} ms",
            self.label, event, name, wall_ms, phase_ms
        );
        tracing::info!(target: "fbuild_build::perf_log", "{}", line);
        eprintln!("{}", line);
    }
}

impl Drop for PerfTimer {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let total = self.start.elapsed();
        let mut summary = format!("[perf-log {}] ", self.label);
        for p in &self.phases {
            summary.push_str(&format!("{}={} ms, ", p.name, p.total.as_millis()));
        }
        summary.push_str(&format!("total={} ms", total.as_millis()));
        // Mirror to both tracing and stderr so the output is always visible
        // regardless of whether a tracing subscriber is attached.
        tracing::info!(target: "fbuild_build::perf_log", "{}", summary);
        eprintln!("{}", summary);
        if let Some(path) = json_sink_path() {
            let value = self.summary_json(total);
            if let Err(e) = append_json_line(path, &value) {
                tracing::warn!(
                    target: "fbuild_build::perf_log",
                    "failed to append perf JSON to {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }
}

/// RAII guard returned by [`PerfTimer::phase`]. Records duration on drop.
pub struct PhaseGuard<'a> {
    owner: &'a mut PerfTimer,
    name: &'static str,
    start: Instant,
}

impl<'a> Drop for PhaseGuard<'a> {
    fn drop(&mut self) {
        if !self.owner.active {
            return;
        }
        let dur = self.start.elapsed();
        if let Some(p) = self.owner.phases.iter_mut().find(|p| p.name == self.name) {
            p.total += dur;
        } else {
            self.owner.phases.push(Phase {
                name: self.name,
                total: dur,
            });
        }
        self.owner.emit_event("phase-finish", self.name, dur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl PerfTimer {
        fn new_active_for_test(label: &'static str) -> Self {
            Self {
                label,
                start: Instant::now(),
                phases: Vec::new(),
                active: true,
            }
        }
    }

    #[test]
    fn summary_json_contains_phases_and_total() {
        let mut t = PerfTimer::new_active_for_test("avr-orchestrator");
        t.record("compile-core", Duration::from_millis(12));
        t.record("link", Duration::from_millis(5));
        let v = t.summary_json(Duration::from_millis(40));
        t.active = false; // suppress drop output
        assert_eq!(v["label"], "avr-orchestrator");
        assert_eq!(v["phases"]["compile-core"].as_f64(), Some(12.0));
        assert_eq!(v["phases"]["link"].as_f64(), Some(5.0));
        assert_eq!(v["total_ms"].as_f64(), Some(40.0));
        assert!(v["unix_ms"].as_u64().unwrap_or(0) > 0);
    }

    #[test]
    fn json_sink_appends_one_line_per_timer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("perf.jsonl");
        append_json_line(&path, &serde_json::json!({"label": "a", "total_ms": 1.0})).unwrap();
        append_json_line(&path, &serde_json::json!({"label": "b", "total_ms": 2.0})).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["label"], "a");
        assert_eq!(lines[1]["label"], "b");
    }

    #[test]
    fn phase_guard_records_duration() {
        // Force active=true to exercise the record path without relying on env.
        let mut t = PerfTimer {
            label: "test",
            start: Instant::now(),
            phases: Vec::new(),
            active: true,
        };
        {
            let _g = t.phase("phase-a");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(t.phases.len(), 1);
        assert_eq!(t.phases[0].name, "phase-a");
        assert!(t.phases[0].total.as_millis() >= 4);
    }

    #[test]
    fn disabled_timer_records_nothing() {
        let mut t = PerfTimer {
            label: "test",
            start: Instant::now(),
            phases: Vec::new(),
            active: false,
        };
        {
            let _g = t.phase("phase-a");
            std::thread::sleep(Duration::from_millis(2));
        }
        t.record("phase-b", Duration::from_millis(10));
        assert!(t.phases.is_empty());
    }

    #[test]
    fn repeated_phase_name_accumulates() {
        let mut t = PerfTimer {
            label: "test",
            start: Instant::now(),
            phases: Vec::new(),
            active: true,
        };
        t.record("x", Duration::from_millis(3));
        t.record("x", Duration::from_millis(4));
        assert_eq!(t.phases.len(), 1);
        assert_eq!(t.phases[0].total.as_millis(), 7);
    }
}
