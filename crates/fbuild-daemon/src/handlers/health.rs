//! Health check, daemon info, and shutdown handlers.

use crate::context::DaemonContext;
use crate::models::{DaemonInfoResponse, HealthResponse, ShutdownParams, ShutdownResponse};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// GET /health
pub async fn health_check(State(ctx): State<Arc<DaemonContext>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        uptime_seconds: ctx.started_at.elapsed().as_secs_f64(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
    })
}

/// GET /api/daemon/info
pub async fn daemon_info(State(ctx): State<Arc<DaemonContext>>) -> Json<DaemonInfoResponse> {
    Json(DaemonInfoResponse {
        status: "running".to_string(),
        uptime_seconds: ctx.started_at.elapsed().as_secs_f64(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        port: ctx.port,
        started_at: ctx.started_at_unix,
        dev_mode: fbuild_paths::is_dev_mode(),
        host: "127.0.0.1".to_string(),
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
