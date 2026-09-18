//! `POST /api/install-deps` — fetch toolchains, frameworks, and libraries
//! without building.

use super::common::{OperationGuard, resolve_request_project_dir};
use crate::context::DaemonContext;
use crate::models::{InstallDepsRequest, OperationResponse};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use std::sync::Arc;

/// POST /api/install-deps
///
/// Install toolchain, framework, and library dependencies without building.
/// Matches the Python daemon's `/api/install-deps` endpoint contract.
pub async fn install_deps(
    State(ctx): State<Arc<DaemonContext>>,
    Json(req): Json<InstallDepsRequest>,
) -> (StatusCode, Json<OperationResponse>) {
    let request_id = req
        .request_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let project_dir = resolve_request_project_dir(&req.project_dir, req.caller_cwd.as_deref());

    if !project_dir.exists() {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!("project directory does not exist: {}", req.project_dir),
            )),
        );
    }

    let _op_guard = OperationGuard::new(
        &ctx,
        fbuild_core::DaemonState::Building,
        Some(format!("Installing deps for {}", req.project_dir)),
    );

    // Acquire per-project lock.
    // FastLED/fbuild#808 (CRITICAL): hard ceiling so a wedged previous
    // build / deploy / install cannot leave install-deps waiting forever.
    const INSTALL_DEPS_LOCK_HARD_DEADLINE: std::time::Duration =
        std::time::Duration::from_secs(30 * 60);
    let lock = ctx.project_lock(&project_dir);
    let _guard = match tokio::time::timeout(INSTALL_DEPS_LOCK_HARD_DEADLINE, lock.lock()).await {
        Ok(g) => g,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(OperationResponse::fail(
                    request_id,
                    format!(
                        "project lock not acquired within {}s; previous build may be wedged — \
                         run `fbuild daemon locks` to see who is holding it",
                        INSTALL_DEPS_LOCK_HARD_DEADLINE.as_secs()
                    ),
                )),
            );
        }
    };

    // Parse platformio.ini to determine platform and resolve packages
    let config =
        match fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini")) {
            Ok(c) => c,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("failed to parse platformio.ini: {}", e),
                    )),
                );
            }
        };

    let env_name = req
        .environment
        .clone()
        .or_else(|| config.get_default_environment().map(|s| s.to_string()))
        .unwrap_or_else(|| "default".to_string());

    if let Err(e) = config.get_env_config(&env_name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!("invalid environment '{}': {}", env_name, e),
            )),
        );
    }

    // Provision everything the env's build downloads — the same set
    // `fbuild install` reports (FastLED/fbuild#1433).
    let result = fbuild_build::provision_env(
        &project_dir,
        &env_name,
        fbuild_build::provision::ProvisionMode::Install,
    )
    .await;

    match result {
        Ok(report) if report.failed() => {
            let failures = report
                .packages
                .iter()
                .filter(|p| p.status == fbuild_build::provision::ProvisionStatus::Failed)
                .map(|p| {
                    format!(
                        "{}: {}",
                        p.name,
                        p.error.as_deref().unwrap_or("unknown error")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("install-deps error: {failures}"),
                )),
            )
        }
        Ok(_) => (
            StatusCode::OK,
            Json(OperationResponse::ok(
                request_id,
                format!("Dependencies installed for environment '{}'", env_name),
            )),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("install-deps error: {}", e),
            )),
        ),
    }
}
