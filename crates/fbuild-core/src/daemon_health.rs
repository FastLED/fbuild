//! Daemon `/health` readiness states, shared by every client that spawns or
//! adopts `fbuild-daemon` (FastLED/fbuild#1480).
//!
//! The daemon binds its endpoint before its heavy initialization and answers
//! `/health` with `503 {"status": "starting", "phase": ...}` until that
//! initialization finishes. A client that sees `starting` knows a daemon owns
//! the endpoint and is making progress, so it keeps waiting instead of
//! counting a fixed spawn budget down and reporting a healthy-but-late daemon
//! as a failure (FastLED/fbuild#1462).
//!
//! Old daemons never report `starting`, and old clients treat the `503` as
//! "not healthy yet", so both version skews degrade to the previous behavior.

use serde::Deserialize;
use std::time::{Duration, Instant};

/// `status` a daemon reports once it serves requests.
pub const HEALTHY_STATUS: &str = "healthy";

/// `status` a daemon reports (with HTTP 503) while it is still initializing.
pub const STARTING_STATUS: &str = "starting";

/// Upper bound on how long a client waits for a daemon that keeps reporting
/// `starting`, measured from the first `starting` answer. Generous on purpose:
/// the phases behind it (USB catalogue fetch, root-ownership lock, embedded
/// zccache bring-up) each have their own shorter deadlines, and a daemon that
/// fails one of them exits, which ends the wait early.
pub const STARTING_BUDGET: Duration = Duration::from_secs(120);

/// Interval between `/health` polls.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Longest a daemon told to terminate (SIGTERM) waits for in-flight
/// operations before it exits anyway. New operations are refused from the
/// moment the signal arrives.
pub const SHUTDOWN_DRAIN_BUDGET: Duration = Duration::from_secs(5);

/// Cap on the daemon's final zccache flush. A normal flush takes well under
/// 100 ms; the cap only matters when zccache is stuck behind a slow disk or a
/// startup load (zackees/zccache#1652).
pub const EXIT_FLUSH_BUDGET: Duration = Duration::from_secs(4);

/// Longest a terminated daemon can take to exit: drain, then flush. Clients
/// that send SIGTERM must wait at least this long before escalating to a
/// forced kill, or the kill lands mid-flush.
pub const TERMINATE_EXIT_BUDGET: Duration =
    Duration::from_secs(SHUTDOWN_DRAIN_BUDGET.as_secs() + EXIT_FLUSH_BUDGET.as_secs());

/// What one `/health` probe says about the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonHealth {
    /// The daemon serves requests.
    Healthy,
    /// A daemon owns the endpoint and is still initializing.
    Starting {
        /// Initialization phase the daemon reported, if any.
        phase: Option<String>,
    },
    /// No answer, or an answer that is neither healthy nor starting.
    Unreachable,
}

#[derive(Deserialize)]
struct StatusBody {
    status: String,
    #[serde(default)]
    phase: Option<String>,
}

/// Classify a `/health` response from its HTTP status and body.
pub fn classify(status: u16, body: &[u8]) -> DaemonHealth {
    if (200..300).contains(&status) {
        return DaemonHealth::Healthy;
    }
    if status == 503 {
        if let Ok(body) = serde_json::from_slice::<StatusBody>(body) {
            if body.status == STARTING_STATUS {
                return DaemonHealth::Starting { phase: body.phase };
            }
        }
    }
    DaemonHealth::Unreachable
}

/// Probe `health_url` once, giving the request `timeout` to answer.
pub async fn probe(client: &reqwest::Client, health_url: &str, timeout: Duration) -> DaemonHealth {
    let Ok(resp) = client.get(health_url).timeout(timeout).send().await else {
        return DaemonHealth::Unreachable;
    };
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return DaemonHealth::Healthy;
    }
    let body = resp.bytes().await.unwrap_or_default();
    classify(status, &body)
}

/// Deadline for a readiness wait that stretches while the daemon reports
/// progress.
///
/// Starts at `now + base`. Each `starting` answer pushes the deadline to
/// `base` past that answer, capped at `starting_budget` past the first
/// `starting` answer. The deadline never moves earlier, so a daemon that stops
/// answering after `starting` (it exited, e.g. on a fatal init error) gets at
/// most `base` more before the wait gives up.
#[derive(Debug, Clone)]
pub struct ReadinessDeadline {
    base: Duration,
    starting_budget: Duration,
    deadline: Instant,
    cap: Option<Instant>,
}

impl ReadinessDeadline {
    pub fn new(now: Instant, base: Duration, starting_budget: Duration) -> Self {
        Self {
            base,
            starting_budget,
            deadline: now + base,
            cap: None,
        }
    }

    /// Record one probe result taken at `now`.
    pub fn observe(&mut self, now: Instant, health: &DaemonHealth) {
        if !matches!(health, DaemonHealth::Starting { .. }) {
            return;
        }
        let cap = *self.cap.get_or_insert(now + self.starting_budget);
        self.deadline = self.deadline.max((now + self.base).min(cap));
    }

    pub fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }
}

/// Result of [`wait_until_healthy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    Healthy,
    /// The deadline passed; `last` is the final probe result.
    TimedOut {
        last: DaemonHealth,
    },
}

/// Poll `health_url` (each probe bounded by `probe_timeout`) until the daemon
/// is healthy or the [`ReadinessDeadline`] built from `base` and
/// [`STARTING_BUDGET`] expires.
///
/// `on_starting` runs for every `starting` answer with the reported phase and
/// the time waited so far, so callers can surface progress.
pub async fn wait_until_healthy(
    client: &reqwest::Client,
    health_url: &str,
    base: Duration,
    probe_timeout: Duration,
    on_starting: impl FnMut(Option<&str>, Duration),
) -> WaitOutcome {
    wait_until_healthy_with(
        client,
        health_url,
        base,
        probe_timeout,
        STARTING_BUDGET,
        on_starting,
    )
    .await
}

async fn wait_until_healthy_with(
    client: &reqwest::Client,
    health_url: &str,
    base: Duration,
    probe_timeout: Duration,
    starting_budget: Duration,
    mut on_starting: impl FnMut(Option<&str>, Duration),
) -> WaitOutcome {
    let started = Instant::now();
    let mut deadline = ReadinessDeadline::new(started, base, starting_budget);
    loop {
        let health = probe(client, health_url, probe_timeout).await;
        if health == DaemonHealth::Healthy {
            return WaitOutcome::Healthy;
        }
        let now = Instant::now();
        deadline.observe(now, &health);
        if let DaemonHealth::Starting { phase } = &health {
            on_starting(phase.as_deref(), now - started);
        }
        if deadline.expired(now) {
            return WaitOutcome::TimedOut { last: health };
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const SECOND: Duration = Duration::from_secs(1);

    fn starting() -> DaemonHealth {
        DaemonHealth::Starting {
            phase: Some("compile_backend".to_string()),
        }
    }

    #[test]
    fn classify_success_is_healthy() {
        assert_eq!(classify(200, b"{}"), DaemonHealth::Healthy);
    }

    #[test]
    fn classify_503_starting_body_is_starting_with_phase() {
        let body = br#"{"status":"starting","phase":"usb_overlay","pid":7}"#;
        assert_eq!(
            classify(503, body),
            DaemonHealth::Starting {
                phase: Some("usb_overlay".to_string())
            }
        );
    }

    #[test]
    fn classify_503_without_starting_status_is_unreachable() {
        assert_eq!(classify(503, b"busy"), DaemonHealth::Unreachable);
        assert_eq!(
            classify(503, br#"{"status":"healthy"}"#),
            DaemonHealth::Unreachable
        );
    }

    #[test]
    fn classify_other_errors_are_unreachable() {
        assert_eq!(
            classify(500, br#"{"status":"starting"}"#),
            DaemonHealth::Unreachable
        );
    }

    #[test]
    fn deadline_without_starting_is_the_base_budget() {
        let t0 = Instant::now();
        let mut d = ReadinessDeadline::new(t0, 10 * SECOND, 120 * SECOND);
        d.observe(t0 + 5 * SECOND, &DaemonHealth::Unreachable);
        assert!(!d.expired(t0 + 9 * SECOND));
        assert!(d.expired(t0 + 10 * SECOND));
    }

    #[test]
    fn deadline_stretches_while_starting() {
        let t0 = Instant::now();
        let mut d = ReadinessDeadline::new(t0, 10 * SECOND, 120 * SECOND);
        for s in (0..=45).step_by(5) {
            d.observe(t0 + s * SECOND, &starting());
        }
        // 47 s of bring-up (the FastLED/fbuild#1462 trace) no longer times out.
        assert!(!d.expired(t0 + 47 * SECOND));
        assert!(d.expired(t0 + 55 * SECOND));
    }

    #[test]
    fn deadline_is_capped_by_the_starting_budget() {
        let t0 = Instant::now();
        let mut d = ReadinessDeadline::new(t0, 10 * SECOND, 30 * SECOND);
        for s in 1..=100 {
            d.observe(t0 + s * SECOND, &starting());
        }
        // Capped at first `starting` (t0+1) + 30 s.
        assert!(!d.expired(t0 + 30 * SECOND));
        assert!(d.expired(t0 + 31 * SECOND));
    }

    #[test]
    fn deadline_never_moves_earlier() {
        let t0 = Instant::now();
        let mut d = ReadinessDeadline::new(t0, 10 * SECOND, SECOND);
        d.observe(t0, &starting());
        // The starting cap (t0+1s) is earlier than the base deadline (t0+10s).
        assert!(!d.expired(t0 + 9 * SECOND));
    }

    /// Minimal HTTP/1.1 responder: answers the first `starting_answers`
    /// requests with `503 starting`, then `200 healthy`.
    async fn serve(starting_answers: usize) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/health", listener.local_addr().unwrap());
        let served = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&served);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let (status, body) = if n < starting_answers {
                    (
                        "503 Service Unavailable",
                        r#"{"status":"starting","phase":"compile_backend"}"#,
                    )
                } else {
                    ("200 OK", r#"{"status":"healthy"}"#)
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        (url, served)
    }

    #[tokio::test]
    async fn wait_outlasts_the_base_budget_while_the_daemon_reports_starting() {
        // ~20 starting answers at 100 ms polls is ~2 s, far past the 300 ms
        // base budget that would have given up before FastLED/fbuild#1480.
        let (url, served) = serve(20).await;
        let mut phases = Vec::new();
        let outcome = wait_until_healthy_with(
            crate::http::client(),
            &url,
            Duration::from_millis(300),
            crate::time::SHORT_HTTP_TIMEOUT,
            30 * SECOND,
            |phase, _| phases.push(phase.map(str::to_string)),
        )
        .await;
        assert_eq!(outcome, WaitOutcome::Healthy);
        assert_eq!(served.load(Ordering::SeqCst), 21);
        assert_eq!(phases.len(), 20);
        assert_eq!(phases[0].as_deref(), Some("compile_backend"));
    }

    #[tokio::test]
    async fn wait_on_a_daemon_stuck_starting_ends_at_the_starting_budget() {
        let (url, _) = serve(usize::MAX).await;
        let started = Instant::now();
        let outcome = wait_until_healthy_with(
            crate::http::client(),
            &url,
            Duration::from_millis(300),
            crate::time::SHORT_HTTP_TIMEOUT,
            Duration::from_millis(800),
            |_, _| {},
        )
        .await;
        assert_eq!(
            outcome,
            WaitOutcome::TimedOut {
                last: DaemonHealth::Starting {
                    phase: Some("compile_backend".to_string())
                }
            }
        );
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(800), "{waited:?}");
        assert!(waited < 5 * SECOND, "{waited:?}");
    }

    #[tokio::test]
    async fn wait_gives_up_on_an_unreachable_endpoint_after_the_base_budget() {
        // Bind then drop, so the port is (almost certainly) refused.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let started = Instant::now();
        let outcome = wait_until_healthy_with(
            crate::http::client(),
            &format!("http://127.0.0.1:{port}/health"),
            Duration::from_millis(300),
            crate::time::SHORT_HTTP_TIMEOUT,
            30 * SECOND,
            |_, _| panic!("an unreachable endpoint never reports starting"),
        )
        .await;
        assert_eq!(
            outcome,
            WaitOutcome::TimedOut {
                last: DaemonHealth::Unreachable
            }
        );
        assert!(started.elapsed() < 5 * SECOND);
    }
}
