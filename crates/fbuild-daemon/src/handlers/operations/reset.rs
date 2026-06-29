//! `POST /api/reset` — toggle DTR/RTS to reset a board.

use super::common::OperationGuard;
use crate::context::DaemonContext;
use crate::models::{OperationResponse, ResetRequest};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

/// POST /api/reset
///
/// Reset a device via serial port DTR/RTS toggling.
/// Uses platform-specific reset sequence based on board identifier.
pub async fn reset(
    State(ctx): State<Arc<DaemonContext>>,
    Json(req): Json<ResetRequest>,
) -> (StatusCode, Json<OperationResponse>) {
    let request_id = req
        .request_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let port = req.port.clone();
    let verbose = req.verbose;

    let platform = req
        .board
        .as_deref()
        .map(fbuild_deploy::reset::detect_platform_for_reset)
        .unwrap_or("generic");

    let _op_guard = OperationGuard::new(
        &ctx,
        fbuild_core::DaemonState::Deploying,
        Some(format!("Resetting device on {}", port)),
    );

    // Preempt serial if someone is monitoring this port
    let _ = ctx
        .serial_manager
        .preempt_for_deploy(&port, "reset".to_string(), request_id.clone())
        .await;

    let platform_str = platform.to_string();
    // FastLED/fbuild#808 (CRITICAL): DTR/RTS toggling is fundamentally
    // a fast operation, but a wedged Windows USB CDC driver can stall
    // a serial open forever. 10 s is more than enough headroom for a
    // legitimate reset; anything longer is a driver failure.
    const RESET_HARD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
    let result = tokio::time::timeout(
        RESET_HARD_DEADLINE,
        tokio::task::spawn_blocking(move || {
            fbuild_deploy::reset::reset_device(&platform_str, &port, verbose)
        }),
    )
    .await;

    // Clear preemption
    ctx.serial_manager.clear_preemption(&req.port).await;

    let result = match result {
        Ok(inner) => inner,
        Err(_) => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                Json(OperationResponse::fail(
                    request_id,
                    format!(
                        "reset on {} exceeded {}s — serial driver may be wedged",
                        req.port,
                        RESET_HARD_DEADLINE.as_secs()
                    ),
                )),
            );
        }
    };

    match result {
        Ok(Ok(true)) => (
            StatusCode::OK,
            Json(OperationResponse::ok(
                request_id,
                format!("device reset successful on {}", req.port),
            )),
        ),
        Ok(Ok(false)) => (
            StatusCode::OK,
            Json(OperationResponse::fail(
                request_id,
                format!("device reset reported failure on {}", req.port),
            )),
        ),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("reset error: {}", e),
            )),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("reset task panicked: {}", e),
            )),
        ),
    }
}
