//! Lock status and management handlers.

use crate::context::DaemonContext;
use crate::models::{
    ClearLocksRequest, ClearLocksResponse, LockStatusResponse, PendingSerialAttachLockInfo,
    PortLockInfo, ProjectLockInfo, SerialClientLockInfo,
};
use axum::extract::State;
use axum::Json;
use fbuild_serial::{PortSessionInfo, SerialClientInfo};
use std::sync::Arc;

fn now_unix_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn age_seconds(now: f64, timestamp: f64) -> f64 {
    (now - timestamp).max(0.0)
}

fn optional_age_seconds(now: f64, timestamp: Option<f64>) -> Option<f64> {
    timestamp.map(|ts| age_seconds(now, ts))
}

fn serial_client_lock_info(client: &SerialClientInfo) -> SerialClientLockInfo {
    SerialClientLockInfo {
        client_id: client.client_id.clone(),
        pid: client.metadata.pid,
        process_alive: client.metadata.pid.map(is_pid_alive),
        exe: client.metadata.exe.clone(),
        cwd: client.metadata.cwd.clone(),
        argv: client.metadata.argv.clone(),
    }
}

fn port_matches(actual: &str, requested: &str) -> bool {
    if cfg!(windows) {
        actual.eq_ignore_ascii_case(requested)
    } else {
        actual == requested
    }
}

fn session_matches_request(session: &PortSessionInfo, request: &ClearLocksRequest) -> bool {
    let port_matches_request = request
        .port
        .as_deref()
        .is_some_and(|port| port_matches(&session.port, port));
    let client_matches_request = request.client_id.as_deref().is_some_and(|client_id| {
        session.owner_client_id.as_deref() == Some(client_id)
            || session.writer_client_id.as_deref() == Some(client_id)
            || session
                .reader_client_ids
                .iter()
                .any(|reader| reader == client_id)
            || session
                .clients
                .iter()
                .any(|client| client.client_id == client_id)
    });

    if request.port.is_none() && request.client_id.is_none() {
        request.serial && request.stale
    } else {
        port_matches_request || client_matches_request
    }
}

fn session_stale_reason(session: &PortSessionInfo) -> Option<String> {
    if session.writer_client_id.is_none() && session.reader_count == 0 {
        return Some("no active reader or writer clients".to_string());
    }

    if let Some(owner_id) = session.owner_client_id.as_deref() {
        if let Some(owner) = session.clients.iter().find(|c| c.client_id == owner_id) {
            if let Some(pid) = owner.metadata.pid {
                return if is_pid_alive(pid) {
                    None
                } else {
                    Some(format!("owner pid {pid} is no longer running"))
                };
            }
        }
    }

    let known_pids: Vec<u32> = session
        .clients
        .iter()
        .filter_map(|client| client.metadata.pid)
        .collect();
    if !known_pids.is_empty() && known_pids.iter().all(|pid| !is_pid_alive(*pid)) {
        return Some(format!(
            "all known client pid(s) are gone: {}",
            known_pids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    None
}

fn live_refusal_reason(session: &PortSessionInfo) -> String {
    if let Some(owner_id) = session.owner_client_id.as_deref() {
        if let Some(owner) = session.clients.iter().find(|c| c.client_id == owner_id) {
            if let Some(pid) = owner.metadata.pid {
                if is_pid_alive(pid) {
                    return format!("owner client {owner_id} pid {pid} is still running");
                }
            }
        }
    }
    if session
        .clients
        .iter()
        .any(|client| client.metadata.pid.is_some())
    {
        return "at least one attached client pid is still running".to_string();
    }
    "no stale evidence available for active session".to_string()
}

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid > i32::MAX as u32 {
            return false;
        }
        // SAFETY: kill(pid, 0) probes existence without sending a signal.
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            true
        } else {
            std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
    }
    #[cfg(windows)]
    {
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        type Handle = *mut std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
            fn CloseHandle(handle: Handle) -> i32;
        }
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                false
            } else {
                CloseHandle(h);
                true
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// GET /api/locks/status
pub async fn lock_status(State(ctx): State<Arc<DaemonContext>>) -> Json<LockStatusResponse> {
    let now = now_unix_secs();
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

    let sessions = ctx.serial_manager.get_port_sessions();
    let port_locks: Vec<PortLockInfo> = sessions
        .iter()
        .map(|s| PortLockInfo {
            port: s.port.clone(),
            is_held: s.is_open,
            holder_description: s.owner_client_id.clone(),
            is_open: s.is_open,
            owner_client_id: s.owner_client_id.clone(),
            writer_client_id: s.writer_client_id.clone(),
            reader_count: s.reader_count,
            reader_client_ids: s.reader_client_ids.clone(),
            baud_rate: s.baud_rate,
            started_at: s.started_at,
            session_age_seconds: age_seconds(now, s.started_at),
            last_activity_at: s.last_activity_at,
            last_activity_age_seconds: age_seconds(now, s.last_activity_at),
            last_read_at: s.last_read_at,
            last_read_age_seconds: optional_age_seconds(now, s.last_read_at),
            last_write_at: s.last_write_at,
            last_write_age_seconds: optional_age_seconds(now, s.last_write_at),
            total_bytes_read: s.total_bytes_read,
            total_bytes_written: s.total_bytes_written,
            clients: s.clients.iter().map(serial_client_lock_info).collect(),
        })
        .collect();

    let mut stale_locks: Vec<String> = sessions
        .iter()
        .filter_map(|s| {
            session_stale_reason(s).map(|reason| format!("serial {}: {}", s.port, reason))
        })
        .collect();
    stale_locks.sort();

    let pending_serial_attaches = ctx
        .pending_serial_attach_infos()
        .into_iter()
        .map(|info| PendingSerialAttachLockInfo {
            id: info.id,
            client_id: info.client_id,
            port: info.port,
            started_at: info.started_at,
            age_seconds: age_seconds(now, info.started_at),
        })
        .collect();

    Json(LockStatusResponse {
        success: true,
        port_locks,
        project_locks,
        pending_serial_attaches,
        stale_locks,
    })
}

/// POST /api/locks/clear
pub async fn clear_locks(
    State(ctx): State<Arc<DaemonContext>>,
    request: Option<Json<ClearLocksRequest>>,
) -> Json<ClearLocksResponse> {
    let request = request.map(|Json(req)| req).unwrap_or_default();
    let mut cleared_project_count = 0usize;
    let keys: Vec<_> = ctx.project_locks.iter().map(|e| e.key().clone()).collect();

    for key in keys {
        if let Some(entry) = ctx.project_locks.get(&key) {
            if entry.value().try_lock().is_ok() {
                drop(entry);
                ctx.project_locks.remove(&key);
                cleared_project_count += 1;
            }
        }
    }

    let mut cleared_serial_sessions = Vec::new();
    let mut refused = Vec::new();
    let serial_requested = request.serial
        || request.stale
        || request.force
        || request.port.is_some()
        || request.client_id.is_some();

    if request.force && request.port.is_none() && request.client_id.is_none() {
        refused.push("--force requires --port or --client-id".to_string());
    } else if serial_requested
        && !request.stale
        && request.port.is_none()
        && request.client_id.is_none()
    {
        refused.push("serial cleanup requires --stale, --port, or --client-id".to_string());
    } else if serial_requested {
        let sessions = ctx.serial_manager.get_port_sessions();
        let mut matched_any = false;
        for session in sessions {
            if !session_matches_request(&session, &request) {
                continue;
            }
            matched_any = true;
            let stale_reason = session_stale_reason(&session);
            if stale_reason.is_none() && !request.force {
                refused.push(format!(
                    "refused to close serial {}: {}; pass --force with --port/--client-id to close it",
                    session.port,
                    live_refusal_reason(&session)
                ));
                continue;
            }

            match ctx
                .serial_manager
                .close_port(&session.port, "clear_locks")
                .await
            {
                Ok(()) => cleared_serial_sessions.push(session.port),
                Err(err) => {
                    refused.push(format!("failed to close serial {}: {}", session.port, err))
                }
            }
        }
        if !matched_any {
            refused.push("no matching serial sessions found".to_string());
        }
    }

    let cleared_serial_count = cleared_serial_sessions.len();
    let cleared_count = cleared_project_count + cleared_serial_count;
    let message = if !refused.is_empty() && cleared_count == 0 {
        format!("No locks cleared; {}", refused.join("; "))
    } else if !refused.is_empty() {
        format!("Cleared {} lock(s); {}", cleared_count, refused.join("; "))
    } else if cleared_count > 0 {
        format!("Cleared {} stale lock(s)", cleared_count)
    } else {
        "No stale locks found".to_string()
    };

    Json(ClearLocksResponse {
        success: true,
        cleared_count,
        cleared_project_count,
        cleared_serial_count,
        cleared_serial_sessions,
        refused,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_serial::{SerialClientMetadata, SerialSession};
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

    fn insert_serial_session(ctx: &Arc<DaemonContext>, port: &str, client: &str, pid: Option<u32>) {
        let now = now_unix_secs();
        let mut session = SerialSession::new(port.to_string(), 115200);
        session.is_open = true;
        session.owner_client_id = Some(client.to_string());
        session.writer_client_id = Some(client.to_string());
        session.reader_client_ids.insert(client.to_string());
        session.started_at = now - 10.0;
        session.last_activity_at = now - 5.0;
        session.client_metadata.insert(
            client.to_string(),
            SerialClientMetadata {
                pid,
                exe: Some("python".to_string()),
                cwd: Some("/tmp/fbuild-test".to_string()),
                argv: Some(vec!["python".to_string(), "-".to_string()]),
            },
        );
        ctx.serial_manager.insert_session_for_test(session);
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
        assert!(status.stale_locks.is_empty());
        assert!(status.pending_serial_attaches.is_empty());
        assert_eq!(status.project_locks.len(), 1);
        assert_eq!(
            status.project_locks[0].project_dir,
            project.to_string_lossy()
        );
        assert!(status.project_locks[0].is_held);

        drop(guard);
    }

    #[tokio::test]
    async fn lock_status_reports_serial_owner_metadata_and_age() {
        let ctx = test_context();
        insert_serial_session(&ctx, "COM_TEST_META", "client-1", Some(std::process::id()));

        let Json(status) = lock_status(State(ctx)).await;

        assert_eq!(status.port_locks.len(), 1);
        let lock = &status.port_locks[0];
        assert_eq!(lock.port, "COM_TEST_META");
        assert_eq!(lock.owner_client_id.as_deref(), Some("client-1"));
        assert_eq!(lock.writer_client_id.as_deref(), Some("client-1"));
        assert_eq!(lock.reader_client_ids, vec!["client-1".to_string()]);
        assert!(lock.session_age_seconds >= 0.0);
        assert!(lock.last_activity_age_seconds >= 0.0);
        assert_eq!(lock.clients.len(), 1);
        assert_eq!(lock.clients[0].pid, Some(std::process::id()));
        assert_eq!(lock.clients[0].process_alive, Some(true));
        assert_eq!(lock.clients[0].exe.as_deref(), Some("python"));
    }

    #[tokio::test]
    async fn lock_status_reports_pending_serial_attach_age() {
        let ctx = test_context();
        let id = ctx.begin_pending_serial_attach();
        ctx.update_pending_serial_attach(id, "client-2".to_string(), "COM_PENDING".to_string());

        let Json(status) = lock_status(State(ctx.clone())).await;

        assert_eq!(status.pending_serial_attaches.len(), 1);
        let pending = &status.pending_serial_attaches[0];
        assert_eq!(pending.id, id);
        assert_eq!(pending.client_id.as_deref(), Some("client-2"));
        assert_eq!(pending.port.as_deref(), Some("COM_PENDING"));
        assert!(pending.age_seconds >= 0.0);

        ctx.end_pending_serial_attach(id);
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

        let Json(response) = clear_locks(State(ctx.clone()), None).await;

        assert!(response.success);
        assert_eq!(response.cleared_count, 1);
        assert_eq!(response.cleared_project_count, 1);
        assert_eq!(response.cleared_serial_count, 0);
        assert!(!ctx.project_locks.contains_key(&unheld));
        assert!(ctx.project_locks.contains_key(&held));

        drop(guard);
    }

    #[tokio::test]
    async fn clear_locks_refuses_live_serial_session_without_force() {
        let ctx = test_context();
        insert_serial_session(
            &ctx,
            "COM_TEST_LIVE",
            "client-live",
            Some(std::process::id()),
        );

        let req = ClearLocksRequest {
            serial: true,
            port: Some("COM_TEST_LIVE".to_string()),
            ..Default::default()
        };
        let Json(response) = clear_locks(State(ctx.clone()), Some(Json(req))).await;

        assert_eq!(response.cleared_serial_count, 0);
        assert_eq!(ctx.serial_manager.get_port_sessions().len(), 1);
        assert!(
            response
                .refused
                .iter()
                .any(|msg| msg.contains("still running")),
            "expected live-session refusal, got {:?}",
            response.refused
        );
    }

    #[tokio::test]
    async fn clear_locks_force_closes_targeted_serial_session() {
        let ctx = test_context();
        insert_serial_session(
            &ctx,
            "COM_TEST_FORCE",
            "client-force",
            Some(std::process::id()),
        );

        let req = ClearLocksRequest {
            serial: true,
            port: Some("COM_TEST_FORCE".to_string()),
            force: true,
            ..Default::default()
        };
        let Json(response) = clear_locks(State(ctx.clone()), Some(Json(req))).await;

        assert_eq!(response.cleared_serial_count, 1);
        assert_eq!(
            response.cleared_serial_sessions,
            vec!["COM_TEST_FORCE".to_string()]
        );
        assert!(ctx.serial_manager.get_port_sessions().is_empty());
    }

    #[tokio::test]
    async fn clear_locks_stale_closes_dead_owner_serial_session() {
        let ctx = test_context();
        insert_serial_session(&ctx, "COM_TEST_STALE", "client-stale", Some(u32::MAX));

        let req = ClearLocksRequest {
            serial: true,
            stale: true,
            ..Default::default()
        };
        let Json(response) = clear_locks(State(ctx.clone()), Some(Json(req))).await;

        assert_eq!(response.cleared_serial_count, 1);
        assert!(response.refused.is_empty());
        assert!(ctx.serial_manager.get_port_sessions().is_empty());
    }
}
