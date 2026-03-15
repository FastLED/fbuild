//! Shared daemon state accessible from all handlers.

use dashmap::DashMap;
use fbuild_serial::SharedSerialManager;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

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
    /// Per-project build locks to serialize builds on the same project.
    pub project_locks: DashMap<PathBuf, Arc<Mutex<()>>>,
    /// Shutdown signal sender.
    pub shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl DaemonContext {
    pub fn new(port: u16, shutdown_tx: tokio::sync::watch::Sender<bool>) -> Self {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        Self {
            started_at: Instant::now(),
            started_at_unix: now_unix,
            port,
            serial_manager: Arc::new(SharedSerialManager::new()),
            is_shutting_down: Arc::new(AtomicBool::new(false)),
            operation_in_progress: Arc::new(AtomicBool::new(false)),
            project_locks: DashMap::new(),
            shutdown_tx,
        }
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
        DaemonContext::new(8765, tx)
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
}
