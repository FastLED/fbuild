//! Bounded queueing for a deploy whose port another request holds.
//!
//! Another client's deploy or reset owns a port for the length of its flash.
//! Failing the second deploy at once discarded a build that may have taken
//! many minutes on a loaded host, so the deploy waits for the port instead.
//! While it waits, the daemon's `current_operation` names the holder: the
//! CLI's liveness watchdog prints each stage change, so the user sees who
//! they are queued behind.

use crate::context::DaemonContext;
use fbuild_serial::preemption::PreemptionConflict;
use std::sync::Arc;
use std::time::Duration;

/// Default budget for waiting on a busy port.
const DEFAULT_PORT_WAIT: Duration = Duration::from_secs(900);
/// Overrides the port-wait budget, in seconds (`0` restores fail-fast).
const PORT_WAIT_ENV: &str = "FBUILD_DEPLOY_PORT_WAIT_SECS";
/// How often a still-waiting deploy repeats itself in the daemon log.
const LOG_EVERY: Duration = Duration::from_secs(30);

/// How long a deploy may queue behind another request's hold on its port.
pub(super) fn deploy_port_wait_budget() -> Duration {
    port_wait_budget_from(std::env::var(PORT_WAIT_ENV).ok().as_deref())
}

fn port_wait_budget_from(raw: Option<&str>) -> Duration {
    raw.map(str::trim)
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(DEFAULT_PORT_WAIT, Duration::from_secs)
}

/// Publishes "waiting for port" as the daemon's current operation while a
/// deploy is queued, and restores the previous description when dropped.
pub(super) struct PortWaitReporter {
    ctx: Arc<DaemonContext>,
    previous: Option<String>,
    waiting: bool,
    last_logged: Option<Duration>,
}

impl PortWaitReporter {
    pub(super) fn new(ctx: &Arc<DaemonContext>) -> Self {
        let previous = ctx.current_operation.read().ok().and_then(|op| op.clone());
        Self {
            ctx: Arc::clone(ctx),
            previous,
            waiting: false,
            last_logged: None,
        }
    }

    pub(super) fn report(&mut self, conflict: &PreemptionConflict, waited: Duration) {
        let message = format!(
            "waiting for {} ({}s so far): {}",
            conflict.port,
            waited.as_secs(),
            conflict
        );
        if let Ok(mut op) = self.ctx.current_operation.write() {
            *op = Some(message.clone());
        }
        self.waiting = true;
        let due = self
            .last_logged
            .is_none_or(|at| waited.saturating_sub(at) >= LOG_EVERY);
        if due {
            self.last_logged = Some(waited);
            tracing::info!("deploy queued: {message}");
        }
    }
}

impl Drop for PortWaitReporter {
    fn drop(&mut self) {
        if !self.waiting {
            return;
        }
        if let Ok(mut op) = self.ctx.current_operation.write() {
            *op = self.previous.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_defaults_and_overrides() {
        assert_eq!(port_wait_budget_from(None), DEFAULT_PORT_WAIT);
        assert_eq!(port_wait_budget_from(Some(" 42 ")), Duration::from_secs(42));
        assert_eq!(port_wait_budget_from(Some("0")), Duration::ZERO);
        assert_eq!(port_wait_budget_from(Some("soon")), DEFAULT_PORT_WAIT);
    }
}
