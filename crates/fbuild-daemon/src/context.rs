//! Shared daemon state accessible from all handlers.

use crate::device_manager::DeviceManager;
use crate::status_manager::StatusManager;
use dashmap::DashMap;
use fbuild_core::DaemonState;
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
    /// Active AVR8js sessions keyed by session ID.
    pub avr8js_sessions: DashMap<String, PathBuf>,
    /// Serializes GC runs so background and manual `/api/cache/gc` don't interleave.
    pub gc_mutex: Arc<tokio::sync::Mutex<()>>,
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
            avr8js_sessions: DashMap::new(),
            gc_mutex: Arc::new(tokio::sync::Mutex::new(())),
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
        self.busy_reason().is_none()
    }

    /// Human-readable description of what is keeping the daemon alive, or
    /// `None` if the daemon is idle and eligible for self-eviction.
    ///
    /// Surfaces the specific blocker (operation in progress, open serial
    /// sessions, pending serial attaches) so the self-eviction loop can log
    /// it and users can see why the daemon isn't shutting down. See issue
    /// FastLED/fbuild#51.
    pub fn busy_reason(&self) -> Option<String> {
        let op_running = self
            .operation_in_progress
            .load(std::sync::atomic::Ordering::Relaxed);
        let serial_session_count = self.serial_manager.get_port_sessions().len();
        let pending_attaches = self
            .pending_serial_attaches
            .load(std::sync::atomic::Ordering::Relaxed);

        let mut reasons: Vec<String> = Vec::new();
        if op_running {
            reasons.push("build/deploy operation in progress".to_string());
        }
        if serial_session_count > 0 {
            reasons.push(format!(
                "{} open serial session{}",
                serial_session_count,
                if serial_session_count == 1 { "" } else { "s" }
            ));
        }
        if pending_attaches > 0 {
            reasons.push(format!(
                "{} pending serial attach{}",
                pending_attaches,
                if pending_attaches == 1 { "" } else { "es" }
            ));
        }

        if reasons.is_empty() {
            None
        } else {
            Some(reasons.join(", "))
        }
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

    /// `busy_reason()` returns `None` for a fresh context (no ops, no serial
    /// sessions, no pending attaches) and `is_empty()` is the negation.
    /// Regression guard for FastLED/fbuild#51: a fresh daemon must be
    /// immediately eligible for self-eviction.
    #[test]
    fn busy_reason_none_on_fresh_context() {
        let ctx = make_ctx();
        assert_eq!(ctx.busy_reason(), None);
        assert!(ctx.is_empty());
    }

    /// An in-progress operation is the primary blocker the self-eviction
    /// loop must surface so users can see why the daemon isn't shutting down.
    #[test]
    fn busy_reason_reports_operation_in_progress() {
        let ctx = make_ctx();
        ctx.operation_in_progress.store(true, Ordering::Relaxed);
        let reason = ctx.busy_reason().expect("expected a busy reason");
        assert!(
            reason.contains("operation in progress"),
            "reason should mention the in-progress operation, got: {}",
            reason
        );
        assert!(!ctx.is_empty());
    }

    /// A pending serial attach (WebSocket client mid-handshake) must keep
    /// the daemon alive AND be surfaced in `busy_reason()` so it's visible
    /// in logs if the client gets stuck. See FastLED/fbuild#51.
    #[test]
    fn busy_reason_reports_pending_serial_attach() {
        let ctx = make_ctx();
        ctx.pending_serial_attaches.fetch_add(1, Ordering::Relaxed);
        let reason = ctx.busy_reason().expect("expected a busy reason");
        assert!(
            reason.contains("pending serial attach"),
            "reason should mention the pending attach, got: {}",
            reason
        );
    }

    /// Multiple concurrent blockers should all appear in the report so the
    /// self-eviction log line describes the full picture.
    #[test]
    fn busy_reason_joins_multiple_blockers() {
        let ctx = make_ctx();
        ctx.operation_in_progress.store(true, Ordering::Relaxed);
        ctx.pending_serial_attaches.fetch_add(2, Ordering::Relaxed);
        let reason = ctx.busy_reason().expect("expected a busy reason");
        assert!(
            reason.contains("operation in progress")
                && reason.contains("2 pending serial attaches"),
            "reason should join both blockers, got: {}",
            reason
        );
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
