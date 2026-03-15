//! Build, deploy, and monitor operation handlers.

use crate::context::DaemonContext;
use crate::models::{BuildRequest, DeployRequest, MonitorRequest, OperationResponse};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// RAII guard that sets `operation_in_progress` to true on creation
/// and false on drop.
struct OperationGuard {
    flag: Arc<std::sync::atomic::AtomicBool>,
}

impl OperationGuard {
    fn new(ctx: &DaemonContext) -> Self {
        ctx.operation_in_progress.store(true, Ordering::Relaxed);
        Self {
            flag: Arc::clone(&ctx.operation_in_progress),
        }
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Relaxed);
    }
}

/// POST /api/build
pub async fn build(
    State(ctx): State<Arc<DaemonContext>>,
    Json(req): Json<BuildRequest>,
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

    let _op_guard = OperationGuard::new(&ctx);

    // Acquire per-project lock
    let lock = ctx.project_lock(&project_dir);
    let _guard = lock.lock().await;

    // Parse platformio.ini to determine platform
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

    let profile = match req.profile.as_deref() {
        Some("quick") => fbuild_core::BuildProfile::Quick,
        _ => fbuild_core::BuildProfile::Release,
    };

    let build_dir = fbuild_paths::get_project_build_root(&project_dir);
    let params = fbuild_build::BuildParams {
        project_dir: project_dir.clone(),
        env_name: env_name.clone(),
        clean: req.clean_build,
        profile,
        build_dir,
        verbose: req.verbose,
        jobs: req.jobs,
    };

    // Run build in spawn_blocking since orchestrators do I/O
    let result = tokio::task::spawn_blocking(move || {
        let orchestrator = fbuild_build::get_orchestrator(platform)?;
        orchestrator.build(&params)
    })
    .await;

    match result {
        Ok(Ok(build_result)) => {
            let msg = if build_result.success {
                let size_str = build_result
                    .size_info
                    .as_ref()
                    .map(|s| {
                        format!(
                            " (flash: {} bytes, ram: {} bytes)",
                            s.total_flash, s.total_ram
                        )
                    })
                    .unwrap_or_default();
                format!(
                    "build succeeded in {:.1}s{}",
                    build_result.build_time_secs, size_str
                )
            } else {
                build_result.message.clone()
            };

            let output_file = build_result
                .hex_path
                .or(build_result.elf_path)
                .map(|p| p.to_string_lossy().to_string());

            let code = if build_result.success { 0 } else { 1 };
            (
                StatusCode::OK,
                Json(OperationResponse {
                    success: build_result.success,
                    request_id,
                    message: msg,
                    exit_code: code,
                    output_file,
                }),
            )
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("build error: {}", e),
            )),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("build task panicked: {}", e),
            )),
        ),
    }
}

/// POST /api/deploy
pub async fn deploy(
    State(ctx): State<Arc<DaemonContext>>,
    Json(req): Json<DeployRequest>,
) -> (StatusCode, Json<OperationResponse>) {
    let request_id = req
        .request_id
        .clone()
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

    let _op_guard = OperationGuard::new(&ctx);

    // Parse config
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

    // Build first unless skip_build
    let firmware_path = if req.skip_build {
        // Look for existing firmware in build dir
        let build_dir = fbuild_paths::get_project_build_root(&project_dir);
        let hex = build_dir
            .join(&env_name)
            .join("release")
            .join("firmware.hex");
        let bin = build_dir
            .join(&env_name)
            .join("release")
            .join("firmware.bin");
        if hex.exists() {
            hex
        } else if bin.exists() {
            bin
        } else {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(
                    request_id,
                    "no firmware found; run build first or remove skip_build".to_string(),
                )),
            );
        }
    } else {
        // Run build first
        let lock = ctx.project_lock(&project_dir);
        let _guard = lock.lock().await;

        let build_dir = fbuild_paths::get_project_build_root(&project_dir);
        let params = fbuild_build::BuildParams {
            project_dir: project_dir.clone(),
            env_name: env_name.clone(),
            clean: req.clean_build,
            profile: fbuild_core::BuildProfile::Release,
            build_dir,
            verbose: req.verbose,
            jobs: None,
        };

        let build_result = {
            let p = platform;
            tokio::task::spawn_blocking(move || {
                let orchestrator = fbuild_build::get_orchestrator(p)?;
                orchestrator.build(&params)
            })
            .await
        };

        match build_result {
            Ok(Ok(r)) if r.success => r
                .hex_path
                .unwrap_or_else(|| r.elf_path.unwrap_or_else(|| PathBuf::from("firmware.bin"))),
            Ok(Ok(r)) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("build failed: {}", r.message),
                    )),
                );
            }
            Ok(Err(e)) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("build error: {}", e),
                    )),
                );
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("build task panicked: {}", e),
                    )),
                );
            }
        }
    };

    // Preempt serial if port specified
    let deploy_port_str = req.port.clone();
    if let Some(ref p) = deploy_port_str {
        let _ = ctx
            .serial_manager
            .preempt_for_deploy(p, "deploy".to_string(), request_id.clone())
            .await;
    }

    // Extract board ID before spawn_blocking (env_config borrows config)
    let board_id = env_config
        .get("board")
        .cloned()
        .unwrap_or_else(|| match platform {
            fbuild_core::Platform::Espressif32 => "esp32dev".to_string(),
            fbuild_core::Platform::AtmelAvr => "uno".to_string(),
            _ => "unknown".to_string(),
        });

    // Deploy
    let deploy_env = env_name.clone();
    let deploy_project = project_dir.clone();
    let deploy_port = deploy_port_str.clone();
    let deploy_fw = firmware_path.clone();
    let deploy_result = tokio::task::spawn_blocking(move || {
        let deployer: Box<dyn fbuild_deploy::Deployer> = match platform {
            fbuild_core::Platform::Espressif32 => {
                let board_config =
                    fbuild_config::BoardConfig::from_board_id(&board_id, &Default::default())
                        .unwrap_or_else(|_| {
                            fbuild_config::BoardConfig::from_board_id(
                                "esp32dev",
                                &Default::default(),
                            )
                            .unwrap()
                        });
                Box::new(fbuild_deploy::esp32::Esp32Deployer::from_board_config(
                    &board_config,
                    "0x0",
                    "0x8000",
                    "0x10000",
                    false,
                ))
            }
            fbuild_core::Platform::AtmelAvr => {
                let board_config =
                    fbuild_config::BoardConfig::from_board_id(&board_id, &Default::default())
                        .unwrap_or_else(|_| {
                            fbuild_config::BoardConfig::from_board_id("uno", &Default::default())
                                .unwrap()
                        });
                Box::new(fbuild_deploy::avr::AvrDeployer::from_board_config(
                    &board_config,
                    false,
                ))
            }
            _ => {
                return Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "deployer for {:?} not yet implemented",
                    platform
                )));
            }
        };
        deployer.deploy(
            &deploy_project,
            &deploy_env,
            &deploy_fw,
            deploy_port.as_deref(),
        )
    })
    .await;

    // Clear preemption then wait for USB re-enumeration
    if let Some(ref p) = deploy_port_str {
        ctx.serial_manager.clear_preemption(p).await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    match deploy_result {
        Ok(Ok(r)) => {
            let code = if r.success { 0 } else { 1 };
            (
                StatusCode::OK,
                Json(OperationResponse {
                    success: r.success,
                    request_id,
                    message: r.message,
                    exit_code: code,
                    output_file: Some(firmware_path.to_string_lossy().to_string()),
                }),
            )
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("deploy error: {}", e),
            )),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("deploy task panicked: {}", e),
            )),
        ),
    }
}

/// POST /api/monitor
pub async fn monitor(
    State(ctx): State<Arc<DaemonContext>>,
    Json(req): Json<MonitorRequest>,
) -> (StatusCode, Json<OperationResponse>) {
    let request_id = req
        .request_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let port = req.port.unwrap_or_else(|| "/dev/ttyUSB0".to_string());
    let baud_rate = req.baud_rate.unwrap_or(115200);

    match ctx
        .serial_manager
        .open_port(&port, baud_rate, &request_id)
        .await
    {
        Ok(()) => (
            StatusCode::OK,
            Json(OperationResponse::ok(
                request_id,
                format!("monitoring {} at {} baud", port, baud_rate),
            )),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("failed to open port: {}", e),
            )),
        ),
    }
}
