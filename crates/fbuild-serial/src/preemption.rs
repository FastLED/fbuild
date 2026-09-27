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

impl PreemptionTracker {
    pub fn new() -> Self {
        Self {
            preempted_ports: Arc::new(Mutex::new(HashMap::new())),
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
        let mut ports = self
            .preempted_ports
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        ports.remove(port);
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
}
