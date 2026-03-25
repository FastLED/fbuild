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
