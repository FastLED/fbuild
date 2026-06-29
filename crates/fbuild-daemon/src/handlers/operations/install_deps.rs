//! `POST /api/install-deps` — fetch toolchains, frameworks, and libraries
//! without building.

use super::common::OperationGuard;
use crate::context::DaemonContext;
use crate::models::{InstallDepsRequest, OperationResponse};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::PathBuf;
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
    let project_dir = PathBuf::from(&req.project_dir);

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

    // Acquire per-project lock
    let lock = ctx.project_lock(&project_dir);
    let _guard = lock.lock().await;

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

    let env_config = match config.get_env_config(&env_name) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(
                    request_id,
                    format!("invalid environment '{}': {}", env_name, e),
                )),
            );
        }
    };

    let platform_str = env_config.get("platform").cloned().unwrap_or_default();
    let platform = match fbuild_core::Platform::from_platform_str(&platform_str) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(
                    request_id,
                    format!("unsupported platform: {}", platform_str),
                )),
            );
        }
    };

    // Install dependencies via the package manager
    let env_label = env_name.clone();
    let result = fbuild_build::install_platform_deps(platform, &project_dir).await;

    match result {
        Ok(()) => (
            StatusCode::OK,
            Json(OperationResponse::ok(
                request_id,
                format!("Dependencies installed for environment '{}'", env_label),
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
