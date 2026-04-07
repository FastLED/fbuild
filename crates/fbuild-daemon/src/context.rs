//! Shared daemon state accessible from all handlers.

use crate::device_manager::DeviceManager;
use crate::status_manager::StatusManager;
use dashmap::DashMap;
use fbuild_core::DaemonState;
use fbuild_deploy::firmware_ledger::FirmwareLedger;
use fbuild_serial::SharedSerialManager;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

/// Broadcast hub for WebSocket endpoints (`/ws/status`, `/ws/logs`).
///
/// Uses `tokio::sync::broadcast` channels so multiple WebSocket clients can
/// subscribe independently. Messages are JSON-serialized strings.
pub struct BroadcastHub {
    /// Status updates (daemon state changes, build progress, etc.).
    pub status_tx: tokio::sync::broadcast::Sender<String>,
    /// Log entries from the daemon.
    pub log_tx: tokio::sync::broadcast::Sender<String>,
}

impl Default for BroadcastHub {
    fn default() -> Self {
        Self::new()
    }
}

impl BroadcastHub {
    pub fn new() -> Self {
        let (status_tx, _) = tokio::sync::broadcast::channel(256);
        let (log_tx, _) = tokio::sync::broadcast::channel(256);
        Self { status_tx, log_tx }
    }

    /// Broadcast a status update to all `/ws/status` subscribers.
    pub fn broadcast_status(&self, msg: &str) {
        // Ignore send errors (no subscribers).
        let _ = self.status_tx.send(msg.to_string());
    }

    /// Broadcast a log entry to all `/ws/logs` subscribers.
    pub fn broadcast_log(&self, msg: &str) {
        let _ = self.log_tx.send(msg.to_string());
    }
}

/// Self-eviction timeout: daemon shuts down after this many seconds with
/// 0 clients, 0 operations, and 0 serial sessions.
/// Set to 120s to accommodate validation workflows (deploy + compile + upload).
pub const SELF_EVICTION_TIMEOUT: Duration = Duration::from_secs(120);

/// Fallback idle timeout: daemon shuts down after 12 hours regardless.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(43200);

/// Interval for checking stale project locks.
pub const STALE_LOCK_CHECK_INTERVAL: Duration = Duration::from_secs(60);

use std::time::Duration;

/// Compute the modification time of the running binary (for stale daemon detection).
/// Returns 0.0 if the mtime cannot be determined.
fn compute_binary_mtime() -> f64 {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.metadata().ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Shared state for the daemon, passed to all axum handlers via `State`.
pub struct DaemonContext {
    /// When the daemon started.
    pub started_at: Instant,
    /// Unix timestamp when daemon started (for API responses).
    pub started_at_unix: f64,
    /// Port the daemon is listening on.
    pub port: u16,
    /// Central serial port manager.
    pub serial_manager: Arc<SharedSerialManager>,
    /// Flag for graceful shutdown.
    pub is_shutting_down: Arc<AtomicBool>,
    /// Whether a build/deploy operation is currently in progress.
    pub operation_in_progress: Arc<AtomicBool>,
    /// Number of WebSocket connections currently inside the serial-monitor
    /// handler (e.g. waiting for a port to open). Counted independently of
    /// `serial_manager` sessions because a port may take seconds to open
    /// during USB re-enumeration; without this counter the daemon would
    /// self-evict mid-attach and the client would see a forcibly-closed
    /// WebSocket. See ISSUES.md "Self-eviction during pending serial attach".
    pub pending_serial_attaches: Arc<AtomicUsize>,
    /// Current daemon state (idle, building, deploying, etc.).
    pub daemon_state: Arc<std::sync::RwLock<DaemonState>>,
    /// Description of the current operation (e.g. project dir being built).
    pub current_operation: Arc<std::sync::RwLock<Option<String>>>,
    /// Per-project build locks to serialize builds on the same project.
    pub project_locks: DashMap<PathBuf, Arc<Mutex<()>>>,
    /// Firmware deployment ledger for skip-redeploy optimization.
    pub firmware_ledger: FirmwareLedger,
    /// Device lease manager.
    pub device_manager: DeviceManager,
    /// Persistent status file manager (`daemon_status.json`).
    pub status_manager: StatusManager,
    /// Shutdown signal sender.
    pub shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// Modification time of the daemon binary at startup (for stale detection).
    pub source_mtime: f64,
    /// Last time any request was processed (for idle timeout).
    pub last_activity: Arc<std::sync::Mutex<Instant>>,
    /// Working directory of the client that spawned this daemon.
    pub spawner_cwd: String,
    /// Broadcast hub for WebSocket status/log streaming.
    pub broadcast_hub: BroadcastHub,
}

impl DaemonContext {
    pub fn new(
        port: u16,
        shutdown_tx: tokio::sync::watch::Sender<bool>,
        spawner_cwd: String,
    ) -> Self {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let source_mtime = compute_binary_mtime();
        Self {
            started_at: Instant::now(),
            started_at_unix: now_unix,
            port,
            serial_manager: Arc::new(SharedSerialManager::new()),
            is_shutting_down: Arc::new(AtomicBool::new(false)),
            operation_in_progress: Arc::new(AtomicBool::new(false)),
            pending_serial_attaches: Arc::new(AtomicUsize::new(0)),
            daemon_state: Arc::new(std::sync::RwLock::new(DaemonState::Idle)),
            current_operation: Arc::new(std::sync::RwLock::new(None)),
            project_locks: DashMap::new(),
            firmware_ledger: FirmwareLedger::new(),
            device_manager: DeviceManager::new(),
            status_manager: StatusManager::new(
                fbuild_paths::get_daemon_status_file(),
                std::process::id(),
                now_unix,
            ),
            shutdown_tx,
            source_mtime,
            last_activity: Arc::new(std::sync::Mutex::new(Instant::now())),
            spawner_cwd,
            broadcast_hub: BroadcastHub::new(),
        }
    }

    /// Record that activity occurred (resets idle timers).
    pub fn touch_activity(&self) {
        if let Ok(mut t) = self.last_activity.lock() {
            *t = Instant::now();
        }
    }

    /// Check whether the daemon is completely idle (no ops, no serial sessions,
    /// no pending serial attaches). Pending attaches are counted separately
    /// because a WebSocket client may be in the middle of opening a port; if
    /// we self-evict during that window the client sees a forcibly-closed
    /// WebSocket and the autoresearch lifecycle breaks.
    pub fn is_empty(&self) -> bool {
        let op_running = self
            .operation_in_progress
            .load(std::sync::atomic::Ordering::Relaxed);
        let serial_session_count = self.serial_manager.get_port_sessions().len();
        let pending_attaches = self
            .pending_serial_attaches
            .load(std::sync::atomic::Ordering::Relaxed);
        !op_running && serial_session_count == 0 && pending_attaches == 0
    }

    /// How long since the daemon started.
    pub fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// How long since the last activity (request processed).
    pub fn idle_duration(&self) -> Duration {
        self.last_activity
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    /// Get or create a per-project lock.
    pub fn project_lock(&self, project_dir: &std::path::Path) -> Arc<Mutex<()>> {
        self.project_locks
            .entry(project_dir.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn make_ctx() -> DaemonContext {
        let (tx, _rx) = tokio::sync::watch::channel(false);
        DaemonContext::new(8765, tx, "unknown".to_string())
    }

    #[test]
    fn new_sets_reasonable_defaults() {
        let ctx = make_ctx();
        assert_eq!(ctx.port, 8765);
        assert!(!ctx.is_shutting_down.load(Ordering::Relaxed));
        assert!(!ctx.operation_in_progress.load(Ordering::Relaxed));
    }

    #[test]
    fn started_at_unix_is_reasonable() {
        let ctx = make_ctx();
        // Should be a Unix timestamp after 2020-01-01
        assert!(ctx.started_at_unix > 1_577_836_800.0);
        // Should be within the last minute of now
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert!((now - ctx.started_at_unix).abs() < 60.0);
    }

    #[test]
    fn project_lock_returns_same_lock_for_same_path() {
        let ctx = make_ctx();
        let path = PathBuf::from("/tmp/project");
        let lock1 = ctx.project_lock(&path);
        let lock2 = ctx.project_lock(&path);
        assert!(Arc::ptr_eq(&lock1, &lock2));
    }

    #[test]
    fn project_lock_returns_different_lock_for_different_paths() {
        let ctx = make_ctx();
        let lock1 = ctx.project_lock(&PathBuf::from("/tmp/a"));
        let lock2 = ctx.project_lock(&PathBuf::from("/tmp/b"));
        assert!(!Arc::ptr_eq(&lock1, &lock2));
    }

    /// Issue A follow-up: a streaming serial monitor must keep
    /// `idle_duration()` near zero by calling `touch_activity()` on every
    /// inbound line. Verify the contract: stale -> touch -> fresh.
    #[test]
    fn touch_activity_resets_idle_duration() {
        let ctx = make_ctx();
        // Force last_activity into the past so idle_duration is non-zero.
        {
            let mut t = ctx.last_activity.lock().unwrap();
            *t = Instant::now() - Duration::from_secs(60);
        }
        let stale = ctx.idle_duration();
        assert!(
            stale >= Duration::from_secs(59),
            "expected stale idle_duration ≥59s, got {:?}",
            stale
        );

        ctx.touch_activity();
        let fresh = ctx.idle_duration();
        assert!(
            fresh < Duration::from_secs(1),
            "touch_activity must reset idle_duration to ~0, got {:?}",
            fresh
        );
    }
}
