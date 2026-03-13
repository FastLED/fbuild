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
use std::sync::Arc;
use tokio::sync::Mutex;

/// Tracks which ports are currently preempted by deploy operations.
pub struct PreemptionTracker {
    preempted_ports: Arc<Mutex<HashMap<String, PreemptionInfo>>>,
}

struct PreemptionInfo {
    _reason: String,
    _preempted_by: String,
    _started_at: std::time::Instant,
}

impl PreemptionTracker {
    pub fn new() -> Self {
        Self {
            preempted_ports: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn preempt(&self, port: &str, reason: String, preempted_by: String) {
        let mut ports = self.preempted_ports.lock().await;
        ports.insert(
            port.to_string(),
            PreemptionInfo {
                _reason: reason,
                _preempted_by: preempted_by,
                _started_at: std::time::Instant::now(),
            },
        );
    }

    pub async fn clear(&self, port: &str) {
        let mut ports = self.preempted_ports.lock().await;
        ports.remove(port);
    }

    pub async fn is_preempted(&self, port: &str) -> bool {
        let ports = self.preempted_ports.lock().await;
        ports.contains_key(port)
    }
}

impl Default for PreemptionTracker {
    fn default() -> Self {
        Self::new()
    }
}
