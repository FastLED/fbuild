//! Idle-window tracking for the daemon's self-eviction loop.
//!
//! The maintenance loop in [`super::run`] ticks once a second and must answer
//! one question: "has this daemon been empty for the whole eviction window?"
//! [`try_self_evict`] answers it with an explicit "now", so the window logic is
//! unit-testable without sleeping (FastLED/fbuild#2144).
//!
//! Empty means `DaemonContext::is_empty()` — no active operations, no admitted
//! requests, no serial sessions, no pending attaches. That predicate is *not*
//! the legacy `operation_in_progress` boolean: one finishing operation used to
//! clear the boolean while a sibling build was still running, which let the
//! loop evict the daemon out from under a live compile (FastLED/fbuild#51).
//!
//! Eviction is committed atomically: `try_begin_shutdown` closes operation
//! admission *before* the caller signals shutdown, so no request can be
//! admitted in the gap between the emptiness snapshot and the shutdown.

use crate::context::DaemonContext;

/// Track continuous idle time and close operation admission before self-eviction.
///
/// Returns `true` when the caller must signal shutdown. `empty_since` is the
/// running start of the current empty window; it is cleared whenever the daemon
/// is busy and re-armed (from `now`) after the next quiet tick, so a daemon that
/// just finished a build gets a *fresh* window rather than an already-expired
/// one.
pub fn try_self_evict(
    ctx: &DaemonContext,
    empty_since: &mut Option<std::time::Instant>,
    now: std::time::Instant,
    timeout: std::time::Duration,
) -> bool {
    if !ctx.is_empty() {
        if empty_since.take().is_some() {
            tracing::debug!("Daemon is no longer empty, resetting eviction timer");
        }
        return false;
    }
    let Some(since) = *empty_since else {
        *empty_since = Some(now);
        tracing::info!(
            "Daemon is idle; self-eviction in {:.0}s unless new work arrives",
            timeout.as_secs_f64()
        );
        return false;
    };
    if now.duration_since(since) < timeout {
        return false;
    }
    if ctx.try_begin_shutdown(false).is_none() {
        // An operation was admitted after the idle snapshot above.
        *empty_since = None;
        return false;
    }
    tracing::info!(
        "Self-eviction triggered: daemon empty for {:.1}s, shutting down",
        now.duration_since(since).as_secs_f64()
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::operations::OperationGuard;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn ctx() -> Arc<DaemonContext> {
        let (tx, _rx) = tokio::sync::watch::channel(false);
        Arc::new(DaemonContext::new(0, tx, ".".to_string()))
    }

    #[test]
    fn self_eviction_waits_for_in_flight_operations_and_a_fresh_idle_window() {
        let ctx = ctx();
        let now = Instant::now();
        let timeout = Duration::from_secs(30);
        let mut empty_since = Some(now - timeout);
        let first = OperationGuard::new(&ctx, fbuild_core::DaemonState::Building, None);
        let second = OperationGuard::new(&ctx, fbuild_core::DaemonState::Building, None);
        // A finishing concurrent operation clears the legacy boolean while
        // the remaining guard still represents an in-flight build.
        drop(first);
        assert_eq!(ctx.operation_drain_count(), 1);
        assert!(!ctx.is_empty());
        assert!(!try_self_evict(&ctx, &mut empty_since, now, timeout));
        assert_eq!(empty_since, None);
        assert!(!try_self_evict(
            &ctx,
            &mut empty_since,
            now + timeout,
            timeout
        ));
        assert_eq!(empty_since, None);
        assert!(
            !ctx.is_shutting_down
                .load(std::sync::atomic::Ordering::Acquire)
        );

        // The window restarts from the first quiet tick after the build ends.
        drop(second);
        let now = now + timeout * 2;
        assert!(!try_self_evict(&ctx, &mut empty_since, now, timeout));
        assert_eq!(empty_since, Some(now));
        assert!(!try_self_evict(
            &ctx,
            &mut empty_since,
            now + timeout / 2,
            timeout
        ));
        assert!(try_self_evict(
            &ctx,
            &mut empty_since,
            now + timeout,
            timeout
        ));
        // Admission is closed before the caller signals shutdown.
        assert!(ctx.begin_operation_admission().is_none());
    }

    #[test]
    fn self_eviction_waits_for_admitted_operation_requests() {
        let ctx = ctx();
        let now = Instant::now();
        let timeout = Duration::from_secs(30);
        let mut empty_since = Some(now - timeout);
        let admission = ctx.begin_operation_admission().unwrap();
        assert!(!ctx.is_empty());
        assert!(!try_self_evict(&ctx, &mut empty_since, now, timeout));
        assert_eq!(empty_since, None);
        drop(admission);
        assert!(!try_self_evict(&ctx, &mut empty_since, now, timeout));
        assert!(try_self_evict(
            &ctx,
            &mut empty_since,
            now + timeout,
            timeout
        ));
    }
}
