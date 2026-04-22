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

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Returns `true` when `FBUILD_PERF_LOG=1` (or any non-empty, non-`0` value).
///
/// Cached after the first call so repeated checks are O(1).
pub fn enabled() -> bool {
    static CACHED: AtomicBool = AtomicBool::new(false);
    static INIT: AtomicBool = AtomicBool::new(false);
    if !INIT.load(Ordering::Relaxed) {
        let v = std::env::var("FBUILD_PERF_LOG")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false);
        CACHED.store(v, Ordering::Relaxed);
        INIT.store(true, Ordering::Relaxed);
    }
    CACHED.load(Ordering::Relaxed)
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
