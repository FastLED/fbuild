//! Health check, daemon info, root, and shutdown handlers.

use crate::context::DaemonContext;
use crate::models::{
    DaemonInfoResponse, HealthResponse, RootResponse, ShutdownParams, ShutdownResponse,
};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// GET /
pub async fn root() -> Json<RootResponse> {
    Json(RootResponse {
        message: "fbuild Daemon API".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        health: "/health".to_string(),
    })
}

/// GET /health
pub async fn health_check(State(ctx): State<Arc<DaemonContext>>) -> Json<HealthResponse> {
    ctx.touch_activity();
    Json(HealthResponse {
        status: "healthy".to_string(),
        uptime_seconds: ctx.started_at.elapsed().as_secs_f64(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        source_mtime: ctx.source_mtime,
    })
}

/// GET /api/daemon/info
pub async fn daemon_info(State(ctx): State<Arc<DaemonContext>>) -> Json<DaemonInfoResponse> {
    ctx.touch_activity();
    let daemon_state = *ctx.daemon_state.read().unwrap_or_else(|e| e.into_inner());
    let current_operation = ctx
        .current_operation
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let cache_identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
    Json(DaemonInfoResponse {
        status: "running".to_string(),
        uptime_seconds: ctx.started_at.elapsed().as_secs_f64(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        port: ctx.port,
        started_at: ctx.started_at_unix,
        dev_mode: fbuild_paths::is_dev_mode(),
        host: "127.0.0.1".to_string(),
        operation_in_progress: ctx.operation_in_progress.load(Ordering::Relaxed),
        daemon_state,
        current_operation,
        dependency_install: ctx.dependency_install_snapshot(),
        client_count: ctx.serial_manager.get_port_sessions().len(),
        cache_dir: cache_identity.cache_root.to_string_lossy().to_string(),
        cache_identity: cache_identity.label_value(),
        cache_schema_version: fbuild_paths::running_process::CACHE_SCHEMA_VERSION,
        daemon_dir: fbuild_paths::get_daemon_dir().to_string_lossy().to_string(),
        source_mtime: ctx.source_mtime,
        spawner_cwd: ctx.spawner_cwd.clone(),
        mcp_url: format!("http://127.0.0.1:{}/mcp", ctx.port),
        watch_set_cache: Some(ctx.watch_set_cache.stats()),
    })
}

/// POST /api/daemon/shutdown
pub async fn shutdown(
    State(ctx): State<Arc<DaemonContext>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    query: Query<ShutdownParams>,
) -> (StatusCode, Json<ShutdownResponse>) {
    let force = query.force.unwrap_or(false);
    let caller = ShutdownCaller::from_headers(peer, &headers);

    if !force && ctx.operation_in_progress.load(Ordering::Relaxed) {
        tracing::warn!(
            peer = %caller.peer,
            client_pid = caller.pid.as_deref().unwrap_or("unknown"),
            client_cwd = caller.cwd.as_deref().unwrap_or("unknown"),
            client_exe = caller.exe.as_deref().unwrap_or("unknown"),
            client_argv = caller.argv.as_deref().unwrap_or("unknown"),
            current_operation = current_operation_for_log(&ctx).as_deref().unwrap_or("unknown"),
            "shutdown refused: operation in progress"
        );
        return (
            StatusCode::CONFLICT,
            Json(ShutdownResponse {
                message: "operation in progress; use ?force=true to force shutdown".to_string(),
            }),
        );
    }

    ctx.is_shutting_down.store(true, Ordering::Relaxed);
    let _ = ctx.shutdown_tx.send(true);
    tracing::info!(
        peer = %caller.peer,
        client_pid = caller.pid.as_deref().unwrap_or("unknown"),
        client_cwd = caller.cwd.as_deref().unwrap_or("unknown"),
        client_exe = caller.exe.as_deref().unwrap_or("unknown"),
        client_argv = caller.argv.as_deref().unwrap_or("unknown"),
        force,
        current_operation = current_operation_for_log(&ctx).as_deref().unwrap_or("none"),
        "shutdown requested"
    );
    (
        StatusCode::OK,
        Json(ShutdownResponse {
            message: "shutting down".to_string(),
        }),
    )
}

#[derive(Debug)]
struct ShutdownCaller {
    peer: SocketAddr,
    pid: Option<String>,
    cwd: Option<String>,
    exe: Option<String>,
    argv: Option<String>,
}

impl ShutdownCaller {
    fn from_headers(peer: SocketAddr, headers: &HeaderMap) -> Self {
        Self {
            peer,
            pid: shutdown_header(headers, "x-fbuild-client-pid"),
            cwd: shutdown_header(headers, "x-fbuild-client-cwd"),
            exe: shutdown_header(headers, "x-fbuild-client-exe"),
            argv: shutdown_header(headers, "x-fbuild-client-argv"),
        }
    }
}

fn shutdown_header(headers: &HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn current_operation_for_log(ctx: &DaemonContext) -> Option<String> {
    ctx.current_operation
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn test_context() -> Arc<DaemonContext> {
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        Arc::new(DaemonContext::new(8765, shutdown_tx, "test".to_string()))
    }

    #[test]
    fn shutdown_caller_extracts_client_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-fbuild-client-pid", HeaderValue::from_static("1234"));
        headers.insert(
            "x-fbuild-client-cwd",
            HeaderValue::from_static("C:/work/fastled"),
        );
        headers.insert(
            "x-fbuild-client-exe",
            HeaderValue::from_static("C:/tools/fbuild.exe"),
        );
        headers.insert(
            "x-fbuild-client-argv",
            HeaderValue::from_static("fbuild build"),
        );

        let caller = ShutdownCaller::from_headers("127.0.0.1:5555".parse().unwrap(), &headers);

        assert_eq!(caller.pid.as_deref(), Some("1234"));
        assert_eq!(caller.cwd.as_deref(), Some("C:/work/fastled"));
        assert_eq!(caller.exe.as_deref(), Some("C:/tools/fbuild.exe"));
        assert_eq!(caller.argv.as_deref(), Some("fbuild build"));
    }

    #[tokio::test]
    async fn shutdown_refuses_non_force_when_operation_in_progress() {
        let ctx = test_context();
        ctx.operation_in_progress.store(true, Ordering::Relaxed);
        *ctx.current_operation.write().unwrap() = Some("Building C:/work/fastled".to_string());

        let (status, body) = shutdown(
            State(ctx.clone()),
            ConnectInfo("127.0.0.1:5555".parse().unwrap()),
            HeaderMap::new(),
            Query(ShutdownParams { force: None }),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body.message,
            "operation in progress; use ?force=true to force shutdown"
        );
        assert!(!ctx.is_shutting_down.load(Ordering::Relaxed));
    }
}
