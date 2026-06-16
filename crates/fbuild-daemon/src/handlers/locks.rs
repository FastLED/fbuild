//! Lock status and management handlers.

use crate::context::DaemonContext;
use crate::models::{ClearLocksResponse, LockStatusResponse, PortLockInfo, ProjectLockInfo};
use axum::extract::State;
use axum::Json;
use std::sync::Arc;

/// GET /api/locks/status
pub async fn lock_status(State(ctx): State<Arc<DaemonContext>>) -> Json<LockStatusResponse> {
    // Gather project lock status
    let project_locks: Vec<ProjectLockInfo> = ctx
        .project_locks
        .iter()
        .map(|entry| {
            let is_held = entry.value().try_lock().is_err();
            ProjectLockInfo {
                project_dir: entry.key().to_string_lossy().to_string(),
                is_held,
            }
        })
        .collect();

    // Gather serial port session status
    let sessions = ctx.serial_manager.get_port_sessions();
    let port_locks: Vec<PortLockInfo> = sessions
        .into_iter()
        .map(|s| PortLockInfo {
            port: s.port.clone(),
            is_held: s.is_open,
            holder_description: s.owner_client_id.clone(),
            is_open: s.is_open,
            writer_client_id: s.writer_client_id,
            reader_count: s.reader_count,
        })
        .collect();

    Json(LockStatusResponse {
        success: true,
        port_locks,
        project_locks,
        stale_locks: vec![],
    })
}

/// POST /api/locks/clear
pub async fn clear_locks(State(ctx): State<Arc<DaemonContext>>) -> Json<ClearLocksResponse> {
    // Remove project lock entries that are not currently held
    let mut cleared = 0usize;
    let keys: Vec<_> = ctx.project_locks.iter().map(|e| e.key().clone()).collect();

    for key in keys {
        if let Some(entry) = ctx.project_locks.get(&key) {
            if entry.value().try_lock().is_ok() {
                // Not held — safe to remove the stale entry
                drop(entry);
                ctx.project_locks.remove(&key);
                cleared += 1;
            }
        }
    }

    let message = if cleared > 0 {
        format!("Cleared {} stale lock(s)", cleared)
    } else {
        "No stale locks found".to_string()
    };

    Json(ClearLocksResponse {
        success: true,
        cleared_count: cleared,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::sync::Mutex;

    fn test_context() -> Arc<DaemonContext> {
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        Arc::new(DaemonContext::new(
            0,
            shutdown_tx,
            "lock-handler-test".to_string(),
        ))
    }

    #[tokio::test]
    async fn lock_status_reports_held_project_locks_without_stale_entries() {
        let ctx = test_context();
        let project = PathBuf::from("/tmp/fbuild-held-project");
        let lock = Arc::new(Mutex::new(()));
        let guard = lock.lock().await;
        ctx.project_locks.insert(project.clone(), lock.clone());

        let Json(status) = lock_status(State(ctx)).await;

        assert!(status.success);
        assert!(
            status.stale_locks.is_empty(),
            "stale_locks is reserved for future durable stale-lock detection"
        );
        assert_eq!(status.project_locks.len(), 1);
        assert_eq!(
            status.project_locks[0].project_dir,
            project.to_string_lossy()
        );
        assert!(status.project_locks[0].is_held);

        drop(guard);
    }

    #[tokio::test]
    async fn clear_locks_removes_only_unheld_project_lock_entries() {
        let ctx = test_context();
        let unheld = PathBuf::from("/tmp/fbuild-unheld-project");
        let held = PathBuf::from("/tmp/fbuild-held-project");
        let held_lock = Arc::new(Mutex::new(()));
        let guard = held_lock.lock().await;
        ctx.project_locks
            .insert(unheld.clone(), Arc::new(Mutex::new(())));
        ctx.project_locks.insert(held.clone(), held_lock.clone());

        let Json(response) = clear_locks(State(ctx.clone())).await;

        assert!(response.success);
        assert_eq!(response.cleared_count, 1);
        assert!(!ctx.project_locks.contains_key(&unheld));
        assert!(ctx.project_locks.contains_key(&held));

        drop(guard);
    }
}
