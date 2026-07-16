//! Time bridge — tokio `sleep` + named `Duration` constants.
//!
//! FastLED/fbuild#844 (bridge sweep). All async sleeps in the workspace
//! flow through this module. The matching `ban_std_thread_sleep` dylint
//! forbids `std::thread::sleep` (which blocks the tokio worker).
//!
//! Named `Duration` constants live here so the workspace doesn't have
//! 335 distinct `Duration::from_secs(...)` literals with no shared
//! meaning. The set below covers the top 80% of the audit found in
//! #844 — extend as new categories emerge.

pub use tokio::time::{Duration, interval, sleep, timeout};

// ---- HTTP timeouts ----

/// Short HTTP timeout — fast-fail registry pings, health checks.
pub const SHORT_HTTP_TIMEOUT: Duration = Duration::from_secs(2);

/// Medium HTTP timeout — daemon-internal JSON RPC, status polls.
pub const MEDIUM_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// Long HTTP timeout — toolchain manifest downloads.
pub const LONG_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

// ---- Deploy / build deadlines ----

/// Post-deploy serial-port recovery deadline (3 s fast-poll). See
/// `Deployer::post_deploy_recovery` in `crates/fbuild-deploy/`.
pub const POST_DEPLOY_RECOVERY_DEADLINE: Duration = Duration::from_secs(3);

/// Daemon long-operation cap (30 min). Anything longer than this is a
/// bug, not a slow user — surfaces with a structured timeout error so
/// the daemon doesn't pin a worker indefinitely.
pub const DAEMON_LONG_OP_TIMEOUT: Duration = Duration::from_secs(1800);

/// "Real" build timeout — 15 min. Covers a cold-cache full-tree compile
/// of any board fbuild supports.
pub const REAL_BUILD_TIMEOUT: Duration = Duration::from_secs(900);

// ---- Poll intervals ----

/// 50 ms poll — the lightest poll loop fbuild uses. Reserve for
/// readiness checks (port file appears, daemon is up).
pub const POLL_50MS: Duration = Duration::from_millis(50);

/// 100 ms poll — standard interactive-feel poll.
pub const POLL_100MS: Duration = Duration::from_millis(100);

/// 200 ms poll — background poll where the user isn't actively waiting.
pub const POLL_200MS: Duration = Duration::from_millis(200);

/// 500 ms poll — slow background poll for long-running operations.
pub const POLL_500MS: Duration = Duration::from_millis(500);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_timeouts_are_monotonic() {
        assert!(SHORT_HTTP_TIMEOUT < MEDIUM_HTTP_TIMEOUT);
        assert!(MEDIUM_HTTP_TIMEOUT < LONG_HTTP_TIMEOUT);
    }

    #[test]
    fn poll_intervals_are_monotonic() {
        assert!(POLL_50MS < POLL_100MS);
        assert!(POLL_100MS < POLL_200MS);
        assert!(POLL_200MS < POLL_500MS);
    }

    #[test]
    fn deploy_recovery_is_shorter_than_real_build() {
        assert!(POST_DEPLOY_RECOVERY_DEADLINE < REAL_BUILD_TIMEOUT);
        assert!(REAL_BUILD_TIMEOUT < DAEMON_LONG_OP_TIMEOUT);
    }
}
