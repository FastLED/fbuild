//! Shared daemon state accessible from all handlers.

use crate::device_manager::DeviceManager;
use crate::status_manager::StatusManager;
use dashmap::DashMap;
use fbuild_core::install_status::InstallStatus;
use fbuild_core::DaemonState;
use fbuild_serial::SharedSerialManager;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Weak};
use std::time::Instant;
use tokio::sync::Mutex;

/// Per-firmware-path memo for the deploy-handler SHA-256 image hash.
///
/// Hashing bootloader + partitions + firmware (~2–4 MB) on every
/// warm redeploy is 5–15 ms of wasted work when the build output is
/// unchanged. The deploy handler reads the three files' `mtime` as a
/// cache key — if all three match the memo, it reuses the stored
/// hash instead of re-reading + re-hashing. Cleared implicitly when
/// any file's `mtime` advances (i.e. the next build produced new
/// output).
#[derive(Debug, Clone, Copy)]
pub struct ImageHashMemo {
    pub bootloader_mtime: std::time::SystemTime,
    pub partitions_mtime: std::time::SystemTime,
    pub firmware_mtime: std::time::SystemTime,
    pub hash: [u8; 32],
}

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
/// 30s is enough to bridge the gap between a compile finishing and the
/// next `fbuild` invocation in an interactive workflow, without leaving
/// the daemon holding shell-inherited resources (see #91) for two minutes
/// after a one-shot command returns.
///
/// Overridable at runtime via `FBUILD_SELF_EVICTION_SECS` (useful for
/// benchmarks that need the daemon to stay alive across multiple CLI
/// invocations without re-paying daemon spawn cost).
pub fn self_eviction_timeout() -> Duration {
    std::env::var("FBUILD_SELF_EVICTION_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(SELF_EVICTION_TIMEOUT_DEFAULT)
}

pub const SELF_EVICTION_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);
pub const SELF_EVICTION_TIMEOUT: Duration = SELF_EVICTION_TIMEOUT_DEFAULT;

/// Freshness window for [`crate::watch_set_cache::DaemonWatchSetCache`],
/// read from the `FBUILD_WATCH_SET_CACHE_SECS` env var at daemon
/// startup (#122 follow-up).
///
/// - Unset / unparseable → 2 s (the cache's own default).
/// - Positive integer → that many seconds.
/// - `0` → `Duration::ZERO`, i.e. every entry is stale the instant
///   it's stored. Lets an operator bypass the cache at runtime —
///   useful for A/B-ing a suspected regression without a rebuild.
pub fn watch_set_cache_window_from_env() -> Duration {
    const DEFAULT_SECS: u64 = 2;
    let secs = std::env::var("FBUILD_WATCH_SET_CACHE_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECS);
    Duration::from_secs(secs)
}

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
    /// Latest dependency/package install status emitted by lower-level crates.
    pub dependency_install: Arc<std::sync::RwLock<Option<InstallStatus>>>,
    /// Per-project build locks to serialize builds on the same project.
    pub project_locks: DashMap<PathBuf, Arc<Mutex<()>>>,
    /// Device lease manager.
    ///
    /// Wrapped in `Arc` (FastLED/fbuild#808) so refresh paths that call
    /// the sync `serialport::available_ports()` can be moved off the
    /// tokio worker via `tokio::task::spawn_blocking` without forcing
    /// `DeviceManager: Clone`.
    pub device_manager: Arc<DeviceManager>,
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
    /// Memoized SHA-256 of the ESP32 deploy-image (bootloader +
    /// partitions + firmware) keyed by firmware file path. See
    /// [`ImageHashMemo`]. Cleared entry-by-entry when `mtime` changes.
    pub image_hash_memo: DashMap<PathBuf, ImageHashMemo>,
    /// Daemon-scoped cache for `hash_watch_set_stamps` results so
    /// back-to-back warm builds skip the per-call walk over thousands
    /// of watched files (the dominant cost on warm rebuilds of large
    /// projects — see `docs/PERF_WARM_BUILD.md`). Threaded into
    /// [`fbuild_build::BuildParams::watch_set_cache`] from the build
    /// handler.
    pub watch_set_cache: Arc<crate::watch_set_cache::DaemonWatchSetCache>,
    /// Serializes GC runs so background and manual `/api/cache/gc` don't interleave.
    pub gc_mutex: Arc<tokio::sync::Mutex<()>>,
}

impl DaemonContext {
    pub fn new(
        port: u16,
        shutdown_tx: tokio::sync::watch::Sender<bool>,
        spawner_cwd: String,
    ) -> Self {
        Self::with_hub(port, shutdown_tx, spawner_cwd, BroadcastHub::new())
    }

    /// Construct with a caller-supplied [`BroadcastHub`]. Used by
    /// `main.rs` so the tracing layer can be registered against
    /// `hub.log_tx` before the daemon emits its first
    /// `tracing::info!` (otherwise the layer would need a late-bound
    /// handle through a global OnceLock).
    ///
    /// FastLED/fbuild#800: the embedded zccache backend is owned by
    /// `fbuild_build::compile_backend`'s process-wide `OnceLock` (set
    /// before this constructor runs in `main.rs`); the context does
    /// not duplicate the handle.
    pub fn with_hub(
        port: u16,
        shutdown_tx: tokio::sync::watch::Sender<bool>,
        spawner_cwd: String,
        broadcast_hub: BroadcastHub,
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
            dependency_install: Arc::new(std::sync::RwLock::new(None)),
            project_locks: DashMap::new(),
            device_manager: Arc::new(DeviceManager::new()),
            status_manager: StatusManager::new(
                fbuild_paths::get_daemon_status_file(),
                std::process::id(),
                now_unix,
            ),
            shutdown_tx,
            source_mtime,
            last_activity: Arc::new(std::sync::Mutex::new(Instant::now())),
            spawner_cwd,
            broadcast_hub,
            avr8js_sessions: DashMap::new(),
            image_hash_memo: DashMap::new(),
            watch_set_cache: Arc::new(crate::watch_set_cache::DaemonWatchSetCache::with_max_age(
                watch_set_cache_window_from_env(),
            )),
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

    pub fn install_dependency_status_subscriber(ctx: &Arc<Self>) {
        let weak: Weak<Self> = Arc::downgrade(ctx);
        fbuild_core::install_status::set_install_status_subscriber(move |status| {
            let Some(ctx) = weak.upgrade() else {
                return;
            };
            ctx.set_dependency_install(status);
        });
    }

    pub fn dependency_install_snapshot(&self) -> Option<InstallStatus> {
        self.dependency_install
            .read()
            .ok()
            .and_then(|status| status.clone())
    }

    pub fn clear_dependency_install(&self) {
        if let Ok(mut status) = self.dependency_install.write() {
            *status = None;
        }
        self.broadcast_hub
            .broadcast_status(&self.status_snapshot_json());
    }

    pub fn set_dependency_install(&self, status: InstallStatus) {
        if let Ok(mut current) = self.dependency_install.write() {
            *current = Some(status);
        }
        self.broadcast_hub
            .broadcast_status(&self.status_snapshot_json());
    }

    pub fn status_snapshot_json(&self) -> String {
        let state = self
            .daemon_state
            .read()
            .map(|s| *s)
            .unwrap_or(fbuild_core::DaemonState::Idle);
        let current_op = self.current_operation.read().ok().and_then(|o| o.clone());
        let op_in_progress = self
            .operation_in_progress
            .load(std::sync::atomic::Ordering::Relaxed);
        let dependency_install = self.dependency_install_snapshot();

        serde_json::json!({
            "type": "status",
            "state": state,
            "message": format!("Daemon {}", serde_json::to_value(state).unwrap_or_default().as_str().unwrap_or("unknown")),
            "current_operation": current_op,
            "operation_in_progress": op_in_progress,
            "dependency_install": dependency_install,
            "progress_percent": null,
            "timestamp": now_unix(),
        })
        .to_string()
    }

    /// Get or create a per-project lock.
    pub fn project_lock(&self, project_dir: &std::path::Path) -> Arc<Mutex<()>> {
        self.project_locks
            .entry(project_dir.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn refresh_devices_and_broadcast_serial_moves(&self) {
        // FastLED/fbuild#808: `serialport::available_ports()` is a sync
        // OS-level call. On Windows USB enumeration can stall for
        // seconds under driver contention. Push the sync call off the
        // tokio worker via `spawn_blocking` and cap with a wall-clock
        // timeout so a wedged USB stack cannot lock a worker.
        let dm = Arc::clone(&self.device_manager);
        let join = tokio::task::spawn_blocking(move || dm.refresh_devices());
        match tokio::time::timeout(Duration::from_secs(5), join).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("device refresh task panicked: {}", e),
            Err(_) => {
                tracing::warn!("device refresh exceeded 5s; continuing — USB stack may be wedged")
            }
        }
        self.rebind_recent_device_port_moves().await;
    }

    pub async fn refresh_devices_if_stale_and_broadcast_serial_moves(
        &self,
        max_age: Duration,
    ) -> bool {
        // FastLED/fbuild#808: route the sync enumeration through
        // `spawn_blocking` + a 5 s wall-clock cap, matching the eager
        // refresh path above.
        let dm = Arc::clone(&self.device_manager);
        let join = tokio::task::spawn_blocking(move || dm.refresh_devices_if_stale(max_age));
        let refreshed = match tokio::time::timeout(Duration::from_secs(5), join).await {
            Ok(Ok(refreshed)) => refreshed,
            Ok(Err(e)) => {
                tracing::warn!("device refresh task panicked: {}", e);
                false
            }
            Err(_) => {
                tracing::warn!("device refresh exceeded 5s; continuing — USB stack may be wedged");
                false
            }
        };
        if refreshed {
            self.rebind_recent_device_port_moves().await;
        }
        refreshed
    }

    async fn rebind_recent_device_port_moves(&self) {
        for move_event in self.device_manager.take_recent_port_moves() {
            match self
                .serial_manager
                .rebind_port_session(
                    &move_event.previous_port,
                    &move_event.port,
                    "tracked_serial_move",
                    move_event.serial_number.clone(),
                )
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    self.serial_manager.notify_port_renumbered(
                        &move_event.previous_port,
                        &move_event.port,
                        "tracked_serial_move",
                        move_event.serial_number,
                    );
                }
                Err(err) => {
                    self.serial_manager.notify_port_rebind_failed(
                        &move_event.previous_port,
                        &move_event.port,
                        "open_failed",
                        err.to_string(),
                    );
                    tracing::warn!(
                        previous_port = move_event.previous_port,
                        port = move_event.port,
                        "failed to rebind serial session after tracked port move: {}",
                        err
                    );
                }
            }
        }
    }
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
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

    #[test]
    fn status_snapshot_includes_dependency_install() {
        let ctx = make_ctx();
        ctx.set_dependency_install(fbuild_core::install_status::status(
            "toolchain",
            Some("1.0"),
            fbuild_core::install_status::InstallPhase::WaitingForLock,
            fbuild_core::install_status::InstallRole::Waiter,
            "waiting for toolchain",
            Some(".toolchain.install.lock"),
        ));

        let snap = ctx.status_snapshot_json();
        let json: serde_json::Value = serde_json::from_str(&snap).unwrap();
        assert_eq!(json["dependency_install"]["name"], "toolchain");
        assert_eq!(json["dependency_install"]["phase"], "waiting_for_lock");
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

    /// `FBUILD_WATCH_SET_CACHE_SECS` controls the cache freshness
    /// window (#122). Exhaustive table-driven coverage of the
    /// parse rules so a future refactor can't silently drop one of
    /// the three cases.
    #[test]
    fn watch_set_cache_window_env_rules() {
        let prior = std::env::var("FBUILD_WATCH_SET_CACHE_SECS").ok();

        // Unset → default.
        unsafe { std::env::remove_var("FBUILD_WATCH_SET_CACHE_SECS") };
        assert_eq!(watch_set_cache_window_from_env(), Duration::from_secs(2));

        // Zero → zero duration (disables the cache by making every
        // entry stale on store).
        unsafe { std::env::set_var("FBUILD_WATCH_SET_CACHE_SECS", "0") };
        assert_eq!(watch_set_cache_window_from_env(), Duration::ZERO);

        // Positive integer → literal.
        unsafe { std::env::set_var("FBUILD_WATCH_SET_CACHE_SECS", "15") };
        assert_eq!(watch_set_cache_window_from_env(), Duration::from_secs(15));

        // Garbage → default (graceful degradation, not a panic).
        unsafe { std::env::set_var("FBUILD_WATCH_SET_CACHE_SECS", "not-a-number") };
        assert_eq!(watch_set_cache_window_from_env(), Duration::from_secs(2));

        // Whitespace around a valid value → parsed after trim.
        unsafe { std::env::set_var("FBUILD_WATCH_SET_CACHE_SECS", "  9  ") };
        assert_eq!(watch_set_cache_window_from_env(), Duration::from_secs(9));

        // Restore whatever the ambient shell had set.
        match prior {
            Some(v) => unsafe { std::env::set_var("FBUILD_WATCH_SET_CACHE_SECS", v) },
            None => unsafe { std::env::remove_var("FBUILD_WATCH_SET_CACHE_SECS") },
        }
    }
}
