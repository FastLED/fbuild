//! Deploy preemption protocol.
//!
//! When a deploy operation starts:
//! 1. Force-close the serial session on the target port
//! 2. Notify all attached monitors via "preempted" message
//! 3. esptool/avrdude takes exclusive OS-level port access
//! 4. After flash + reset completes, clear preemption
//! 5. Monitors with auto_reconnect=true automatically reattach
//!
//! Windows USB-CDC timing:
//! - After hard reset, Windows takes 20-30s to re-enumerate the USB device
//! - Use 30 retries with exponential backoff (1s → 2s → 4s → 8s → 10s max)
//! - Detect boot crashes early and trigger hardware reset

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Upper bound between re-checks while waiting for a busy port. A release
/// wakes waiters immediately; this only bounds a missed wake-up.
const PORT_WAIT_RECHECK: Duration = Duration::from_secs(5);

/// Tracks which ports are currently preempted by deploy operations.
///
/// Uses `std::sync::Mutex` (not `tokio::sync::Mutex`) on purpose: no
/// `.await` ever happens inside the critical sections below, so a
/// blocking mutex is both faster and removes the
/// `lock().await`-without-`try_lock_for` foot-gun flagged in
/// FastLED/fbuild#803 MEDIUM. The methods remain `async` to preserve
/// the call-site signature for callers that already `.await` them.
pub struct PreemptionTracker {
    preempted_ports: Arc<Mutex<HashMap<String, PreemptionInfo>>>,
    /// Wakes [`Self::preempt_waiting`] callers when any port is released.
    released: tokio::sync::Notify,
}

#[derive(Debug, Clone)]
pub struct PreemptionInfo {
    pub reason: String,
    pub preempted_by: String,
    pub started_at: std::time::Instant,
}

#[derive(Debug, Clone)]
pub struct PreemptionConflict {
    pub port: String,
    pub holder: PreemptionInfo,
}

impl std::fmt::Display for PreemptionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "port {} is in use: {} by request {} for {}s",
            self.port,
            self.holder.reason,
            self.holder.preempted_by,
            self.holder.started_at.elapsed().as_secs()
        )
    }
}

impl std::error::Error for PreemptionConflict {}

/// A deploy waited its whole budget for a port another request still holds.
#[derive(Debug, Clone)]
pub struct PortWaitTimeout {
    pub conflict: PreemptionConflict,
    pub waited: Duration,
}

impl std::fmt::Display for PortWaitTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "port {} is still in use after waiting {}s: {}",
            self.conflict.port,
            self.waited.as_secs(),
            self.conflict
        )
    }
}

impl std::error::Error for PortWaitTimeout {}

impl PreemptionTracker {
    pub fn new() -> Self {
        Self {
            preempted_ports: Arc::new(Mutex::new(HashMap::new())),
            released: tokio::sync::Notify::new(),
        }
    }

    pub async fn preempt(
        &self,
        port: &str,
        reason: String,
        preempted_by: String,
    ) -> Result<(), PreemptionConflict> {
        // No `.await` inside this critical section — `std::sync::Mutex`
        // is the right choice. If a previous holder panicked, keep the
        // tracker usable and recover the inner map.
        let mut ports = self
            .preempted_ports
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if let Some(holder) = ports.get(port) {
            return Err(PreemptionConflict {
                port: port.to_string(),
                holder: holder.clone(),
            });
        }
        ports.insert(
            port.to_string(),
            PreemptionInfo {
                reason,
                preempted_by,
                started_at: std::time::Instant::now(),
            },
        );
        Ok(())
    }

    pub async fn clear(&self, port: &str) {
        let removed = {
            let mut ports = self
                .preempted_ports
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            ports.remove(port).is_some()
        };
        if removed {
            self.released.notify_waiters();
        }
    }

    /// Like [`Self::preempt`], but a port held by another request is waited
    /// for, up to `budget`, instead of failing at once.
    ///
    /// Another client's deploy legitimately holds a port for the length of
    /// its flash; failing immediately threw away a finished multi-minute
    /// build. `on_wait` is called on every re-check with the current holder
    /// and the time waited so far, so the caller can say who it is queued
    /// behind.
    pub async fn preempt_waiting(
        &self,
        port: &str,
        reason: String,
        preempted_by: String,
        budget: Duration,
        mut on_wait: impl FnMut(&PreemptionConflict, Duration),
    ) -> Result<(), PortWaitTimeout> {
        let started = std::time::Instant::now();
        loop {
            // Register for the wake-up before checking, so a release between
            // the check and the wait is not missed.
            let released = self.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            let conflict = match self
                .preempt(port, reason.clone(), preempted_by.clone())
                .await
            {
                Ok(()) => return Ok(()),
                Err(conflict) => conflict,
            };
            let waited = started.elapsed();
            if waited >= budget {
                return Err(PortWaitTimeout { conflict, waited });
            }
            on_wait(&conflict, waited);
            let recheck = (budget - waited).min(PORT_WAIT_RECHECK);
            let _ = tokio::time::timeout(recheck, released).await;
        }
    }

    pub async fn is_preempted(&self, port: &str) -> bool {
        let ports = self
            .preempted_ports
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        ports.contains_key(port)
    }

    pub async fn holder(&self, port: &str) -> Option<PreemptionInfo> {
        let ports = self
            .preempted_ports
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        ports.get(port).cloned()
    }

    /// Run the serial-session publication step while holding the same mutex
    /// used by deploy/reset acquisition. This closes the attach-vs-deploy
    /// TOCTOU window without holding the lock across the blocking OS open.
    pub fn publish_if_available<T>(
        &self,
        port: &str,
        publish: impl FnOnce() -> T,
    ) -> Result<T, PreemptionConflict> {
        let ports = self
            .preempted_ports
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if let Some(holder) = ports.get(port) {
            return Err(PreemptionConflict {
                port: port.to_string(),
                holder: holder.clone(),
            });
        }
        Ok(publish())
    }
}

impl Default for PreemptionTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::PreemptionTracker;
    use std::sync::{Arc, Barrier};

    #[tokio::test]
    async fn different_request_cannot_replace_active_holder() {
        let tracker = PreemptionTracker::new();
        tracker
            .preempt("COM1", "deploy".into(), "request-1".into())
            .await
            .unwrap();

        let conflict = tracker
            .preempt("COM1", "reset".into(), "request-2".into())
            .await
            .expect_err("a second request must not replace an active deploy");

        assert_eq!(conflict.holder.reason, "deploy");
        assert_eq!(conflict.holder.preempted_by, "request-1");
        assert!(conflict.to_string().contains("port COM1 is in use"));
        let holder = tracker.holder("COM1").await.unwrap();
        assert_eq!(holder.preempted_by, "request-1");
    }

    #[tokio::test]
    async fn same_request_id_cannot_bypass_active_holder() {
        let tracker = PreemptionTracker::new();
        tracker
            .preempt("COM1", "deploy".into(), "request-1".into())
            .await
            .unwrap();
        let conflict = tracker
            .preempt("COM1", "deploy".into(), "request-1".into())
            .await
            .expect_err("request ids are caller supplied and are not lease tokens");

        assert_eq!(conflict.holder.preempted_by, "request-1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deploy_acquisition_waits_for_atomic_serial_publication() {
        let tracker = Arc::new(PreemptionTracker::new());
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let publisher_tracker = Arc::clone(&tracker);
        let publisher_entered = Arc::clone(&entered);
        let publisher_release = Arc::clone(&release);
        let publisher = tokio::task::spawn_blocking(move || {
            publisher_tracker
                .publish_if_available("COM1", || {
                    publisher_entered.wait();
                    publisher_release.wait();
                })
                .unwrap();
        });

        entered.wait();
        let deploy_tracker = Arc::clone(&tracker);
        let deploy = tokio::spawn(async move {
            deploy_tracker
                .preempt("COM1", "deploy".into(), "request-1".into())
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !deploy.is_finished(),
            "deploy must not acquire between availability check and publication"
        );

        release.wait();
        publisher.await.unwrap();
        deploy.await.unwrap().unwrap();
    }
    /// A second deploy whose build finished while another client's deploy
    /// held the port must queue for it, not throw the build away.
    #[tokio::test]
    async fn busy_port_is_waited_on_then_acquired() {
        let tracker = Arc::new(PreemptionTracker::new());
        tracker
            .preempt("COM1", "deploy (project a)".into(), "request-1".into())
            .await
            .unwrap();
        let releaser = Arc::clone(&tracker);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            releaser.clear("COM1").await;
        });

        let mut seen_holders = Vec::new();
        tracker
            .preempt_waiting(
                "COM1",
                "deploy (project b)".into(),
                "request-2".into(),
                std::time::Duration::from_secs(30),
                |conflict, _waited| seen_holders.push(conflict.holder.preempted_by.clone()),
            )
            .await
            .expect("the port is released well inside the wait budget");

        assert_eq!(seen_holders.first().map(String::as_str), Some("request-1"));
        let holder = tracker.holder("COM1").await.unwrap();
        assert_eq!(holder.preempted_by, "request-2");
    }

    /// The wait is bounded: a holder that never lets go fails the second
    /// deploy with an error that still names the holder.
    #[tokio::test]
    async fn busy_port_wait_is_bounded_and_names_the_holder() {
        let tracker = PreemptionTracker::new();
        tracker
            .preempt("COM1", "deploy (project a)".into(), "request-1".into())
            .await
            .unwrap();

        let error = tracker
            .preempt_waiting(
                "COM1",
                "deploy (project b)".into(),
                "request-2".into(),
                std::time::Duration::from_millis(150),
                |_, _| {},
            )
            .await
            .expect_err("a holder that never releases must time the waiter out");

        let message = error.to_string();
        assert!(message.contains("still in use after waiting"), "{message}");
        assert!(message.contains("request-1"), "{message}");
        assert_eq!(
            tracker.holder("COM1").await.unwrap().preempted_by,
            "request-1"
        );
    }
}
