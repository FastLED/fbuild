//! Health check, daemon info, root, and shutdown handlers.

use crate::context::DaemonContext;
use crate::models::{
    DaemonInfoResponse, HealthResponse, RootResponse, ShutdownParams, ShutdownResponse,
};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
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
    query: Query<ShutdownParams>,
) -> (StatusCode, Json<ShutdownResponse>) {
    let force = query.force.unwrap_or(false);

    if !force && ctx.operation_in_progress.load(Ordering::Relaxed) {
        return (
            StatusCode::CONFLICT,
            Json(ShutdownResponse {
                message: "operation in progress; use ?force=true to force shutdown".to_string(),
            }),
        );
    }

    ctx.is_shutting_down.store(true, Ordering::Relaxed);
    let _ = ctx.shutdown_tx.send(true);
    tracing::info!("shutdown requested");
    (
        StatusCode::OK,
        Json(ShutdownResponse {
            message: "shutting down".to_string(),
        }),
    )
}
