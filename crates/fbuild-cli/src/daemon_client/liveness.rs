//! Progress-based liveness for long daemon operations.
//!
//! A deploy or build request used to carry a fixed wall-clock deadline, so on
//! a heavily loaded host a slow but healthy build was indistinguishable from
//! a hung daemon. This module follows soldr's inactivity watchdog
//! (soldr#2974: measure *silence*, not elapsed time): while the request is
//! outstanding the CLI samples the daemon's progress counter, keeps waiting
//! for as long as it advances, and gives up only after a stall window with no
//! progress at all. The window grows with the host's load average per CPU, so
//! a machine running at many times its core count is given proportionally
//! more time before it is called hung.

use std::future::Future;
use std::time::Duration;

/// Default stall window on an unloaded host.
const DEFAULT_STALL_WINDOW: Duration = Duration::from_secs(600);
/// How often the daemon's progress counter is sampled.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(10);
/// The load multiplier never shrinks the window and never stretches it
/// beyond this factor.
const MAX_LOAD_FACTOR: f64 = 16.0;
/// Overrides the base stall window, in seconds.
pub(crate) const STALL_WINDOW_ENV: &str = "FBUILD_DAEMON_STALL_SECS";

/// One sample of the daemon's progress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProgressSample {
    /// Monotonic progress counter. `None` for a daemon that predates it.
    pub seq: Option<u64>,
    /// What the daemon says it is doing right now.
    pub stage: Option<String>,
}

/// Tuning for [`await_with_liveness`].
#[derive(Debug, Clone)]
pub(crate) struct LivenessPolicy {
    pub poll_interval: Duration,
    pub base_stall_window: Duration,
    /// Wall-clock cap used only when the daemon reports no progress counter
    /// (an older daemon): without a signal there is nothing better to judge by.
    pub legacy_ceiling: Duration,
    /// Load average per CPU, or `None` when the host cannot report it.
    pub load_ratio: fn() -> Option<f64>,
}

impl LivenessPolicy {
    pub(crate) fn from_env(legacy_ceiling: Duration) -> Self {
        Self {
            poll_interval: DEFAULT_POLL_INTERVAL,
            base_stall_window: stall_window_from_env(std::env::var(STALL_WINDOW_ENV).ok()),
            legacy_ceiling,
            load_ratio: host_load_ratio,
        }
    }

    /// Stall window for the current load, and the load ratio it used.
    pub(crate) fn stall_window(&self) -> (Duration, Option<f64>) {
        let ratio = (self.load_ratio)();
        let factor = ratio
            .filter(|r| r.is_finite())
            .map_or(1.0, |r| r.clamp(1.0, MAX_LOAD_FACTOR));
        (self.base_stall_window.mul_f64(factor), ratio)
    }
}

fn stall_window_from_env(raw: Option<String>) -> Duration {
    raw.as_deref()
        .map(str::trim)
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map_or(DEFAULT_STALL_WINDOW, Duration::from_secs)
}

/// One-minute load average divided by the CPU count.
fn host_load_ratio() -> Option<f64> {
    let cpus = std::thread::available_parallelism().ok()?.get() as f64;
    load_average_1m().map(|load| load / cpus)
}

#[cfg(target_os = "linux")]
fn load_average_1m() -> Option<f64> {
    parse_loadavg(&std::fs::read_to_string("/proc/loadavg").ok()?)
}

#[cfg(not(target_os = "linux"))]
fn load_average_1m() -> Option<f64> {
    None
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_loadavg(contents: &str) -> Option<f64> {
    contents.split_whitespace().next()?.parse().ok()
}

/// Await `work` while it keeps making progress.
///
/// `probe` samples the daemon every poll interval (`None` when it could not
/// be reached; that is neither progress nor, by itself, a failure: if the
/// daemon is gone the request in `work` fails on its own). `on_stage` is
/// called whenever the reported stage changes, so the caller can show the
/// user what the daemon is waiting on.
pub(crate) async fn await_with_liveness<W, T, P, PF, S>(
    work: W,
    mut probe: P,
    policy: &LivenessPolicy,
    mut on_stage: S,
) -> fbuild_core::Result<T>
where
    W: Future<Output = fbuild_core::Result<T>>,
    P: FnMut() -> PF,
    PF: Future<Output = Option<ProgressSample>>,
    S: FnMut(&str),
{
    tokio::pin!(work);
    let mut watch = ProgressWatch::new(tokio::time::Instant::now());
    loop {
        tokio::select! {
            output = &mut work => return output,
            () = tokio::time::sleep(policy.poll_interval) => {}
        }
        let now = tokio::time::Instant::now();
        if let Some(sample) = probe().await {
            if let Some(stage) = watch.observe(sample, now) {
                on_stage(stage);
            }
        }
        if let Some(error) = watch.verdict(now, policy) {
            return Err(error);
        }
    }
}

/// What the watchdog has seen of the daemon so far.
struct ProgressWatch {
    started: tokio::time::Instant,
    last_progress: tokio::time::Instant,
    last_seq: Option<u64>,
    stage: Option<String>,
}

impl ProgressWatch {
    fn new(now: tokio::time::Instant) -> Self {
        Self {
            started: now,
            last_progress: now,
            last_seq: None,
            stage: None,
        }
    }

    /// Record a sample; returns the new stage when it changed.
    fn observe(&mut self, sample: ProgressSample, now: tokio::time::Instant) -> Option<&str> {
        if sample.seq.is_some() && sample.seq != self.last_seq {
            self.last_seq = sample.seq;
            self.last_progress = now;
        }
        if sample.stage.is_none() || sample.stage == self.stage {
            return None;
        }
        self.stage = sample.stage;
        self.stage.as_deref()
    }

    /// `Some` once the operation should be declared hung.
    fn verdict(
        &self,
        now: tokio::time::Instant,
        policy: &LivenessPolicy,
    ) -> Option<fbuild_core::FbuildError> {
        let stage = self.stage.as_deref();
        if self.last_seq.is_none() {
            let elapsed = now.duration_since(self.started);
            return (elapsed >= policy.legacy_ceiling).then(|| {
                stall_error(
                    elapsed,
                    policy.legacy_ceiling,
                    None,
                    stage,
                    "the daemon reports no progress counter (older daemon)",
                )
            });
        }
        let (window, ratio) = policy.stall_window();
        let silent = now.duration_since(self.last_progress);
        (silent >= window).then(|| stall_error(silent, window, ratio, stage, "no daemon progress"))
    }
}

fn stall_error(
    silent: Duration,
    window: Duration,
    load_ratio: Option<f64>,
    stage: Option<&str>,
    why: &str,
) -> fbuild_core::FbuildError {
    let load = load_ratio.map_or_else(
        || "host load unknown".to_string(),
        |r| format!("host load {r:.1}x its CPU count"),
    );
    fbuild_core::FbuildError::DaemonError(format!(
        "daemon operation stalled during stage '{}': {why} for {}s (stall window {}s, {load}). \
         The daemon appears hung rather than slow; see {}. Raise {STALL_WINDOW_ENV} to wait longer.",
        stage.unwrap_or("unknown"),
        silent.as_secs(),
        window.as_secs(),
        fbuild_paths::get_daemon_log_file().display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn policy(load: fn() -> Option<f64>) -> LivenessPolicy {
        LivenessPolicy {
            poll_interval: Duration::from_secs(10),
            base_stall_window: Duration::from_secs(600),
            legacy_ceiling: fbuild_core::time::DAEMON_LONG_OP_TIMEOUT,
            load_ratio: load,
        }
    }

    fn no_load() -> Option<f64> {
        None
    }

    fn heavy_load() -> Option<f64> {
        Some(4.0)
    }

    /// A build that keeps making progress for longer than the old fixed
    /// 30-minute request deadline must still succeed.
    #[tokio::test(start_paused = true)]
    async fn slow_but_progressing_operation_outlasts_old_fixed_timeout() {
        let seq = Arc::new(AtomicU64::new(0));
        let work = async {
            tokio::time::sleep(fbuild_core::time::DAEMON_LONG_OP_TIMEOUT * 2).await;
            Ok::<_, fbuild_core::FbuildError>(7)
        };
        let out = await_with_liveness(
            work,
            || {
                let seq = Arc::clone(&seq);
                async move {
                    Some(ProgressSample {
                        seq: Some(seq.fetch_add(1, Ordering::SeqCst)),
                        stage: Some("compiling".into()),
                    })
                }
            },
            &policy(no_load),
            |_| {},
        )
        .await;
        assert_eq!(out.unwrap(), 7);
    }

    /// No progress at all for the stall window is a hang, and the error names
    /// the stage the daemon was stuck in.
    #[tokio::test(start_paused = true)]
    async fn stalled_operation_fails_with_clear_error_naming_stage() {
        let started = tokio::time::Instant::now();
        let out = await_with_liveness(
            std::future::pending::<fbuild_core::Result<()>>(),
            || async {
                Some(ProgressSample {
                    seq: Some(42),
                    stage: Some("deploying esp32c6 to /dev/ttyACM1".into()),
                })
            },
            &policy(no_load),
            |_| {},
        )
        .await;
        let message = out.unwrap_err().to_string();
        assert!(message.contains("stalled"), "{message}");
        assert!(
            message.contains("deploying esp32c6 to /dev/ttyACM1"),
            "{message}"
        );
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_secs(600) && waited < Duration::from_secs(700),
            "stall detection should fire one window after the last progress, got {waited:?}"
        );
    }

    /// On a host loaded at four times its CPU count the same silence is
    /// tolerated for four times as long before the operation is called hung.
    #[tokio::test(start_paused = true)]
    async fn high_load_extends_the_stall_window() {
        let work = async {
            tokio::time::sleep(Duration::from_secs(1500)).await;
            Ok::<_, fbuild_core::FbuildError>("done")
        };
        let out = await_with_liveness(
            work,
            || async {
                Some(ProgressSample {
                    seq: Some(1),
                    stage: None,
                })
            },
            &policy(heavy_load),
            |_| {},
        )
        .await;
        assert_eq!(out.unwrap(), "done");
        assert_eq!(
            policy(heavy_load).stall_window().0,
            Duration::from_secs(2400)
        );
    }

    /// Stage changes are surfaced once each, e.g. a deploy queued behind
    /// another client's port lease.
    #[tokio::test(start_paused = true)]
    async fn stage_changes_are_reported() {
        let calls = Arc::new(AtomicU64::new(0));
        let mut seen = Vec::new();
        let work = async {
            tokio::time::sleep(Duration::from_secs(45)).await;
            Ok::<_, fbuild_core::FbuildError>(())
        };
        await_with_liveness(
            work,
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    Some(ProgressSample {
                        seq: Some(n),
                        stage: Some(
                            if n < 2 {
                                "waiting for port"
                            } else {
                                "flashing"
                            }
                            .into(),
                        ),
                    })
                }
            },
            &policy(no_load),
            |s| seen.push(s.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(seen, vec!["waiting for port", "flashing"]);
    }

    /// A daemon too old to report progress keeps the previous wall-clock cap.
    #[tokio::test(start_paused = true)]
    async fn legacy_daemon_without_counter_uses_the_old_ceiling() {
        let started = tokio::time::Instant::now();
        let out = await_with_liveness(
            std::future::pending::<fbuild_core::Result<()>>(),
            || async { Some(ProgressSample::default()) },
            &policy(no_load),
            |_| {},
        )
        .await;
        assert!(out.is_err());
        assert!(started.elapsed() >= fbuild_core::time::DAEMON_LONG_OP_TIMEOUT);
    }

    #[test]
    fn stall_window_env_and_loadavg_parse() {
        assert_eq!(stall_window_from_env(None), DEFAULT_STALL_WINDOW);
        assert_eq!(
            stall_window_from_env(Some(" 90 ".into())),
            Duration::from_secs(90)
        );
        assert_eq!(
            stall_window_from_env(Some("0".into())),
            DEFAULT_STALL_WINDOW
        );
        assert_eq!(
            stall_window_from_env(Some("x".into())),
            DEFAULT_STALL_WINDOW
        );
        assert_eq!(
            parse_loadavg("1148.13 758.79 429.97 285/12844 1"),
            Some(1148.13)
        );
        assert_eq!(parse_loadavg(""), None);
    }
}
