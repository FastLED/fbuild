//! Build, deploy, and monitor operation handlers.

use crate::context::DaemonContext;
use crate::models::{
    BuildRequest, DeployRequest, InstallDepsRequest, MonitorRequest, OperationResponse,
    ResetRequest,
};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// RAII guard that sets `operation_in_progress` to true on creation
/// and false on drop. Also tracks daemon state and current operation description.
struct OperationGuard {
    flag: Arc<std::sync::atomic::AtomicBool>,
    state: Arc<std::sync::RwLock<fbuild_core::DaemonState>>,
    operation: Arc<std::sync::RwLock<Option<String>>>,
}

impl OperationGuard {
    fn new(
        ctx: &DaemonContext,
        daemon_state: fbuild_core::DaemonState,
        description: Option<String>,
    ) -> Self {
        ctx.touch_activity();
        ctx.operation_in_progress.store(true, Ordering::Relaxed);
        if let Ok(mut s) = ctx.daemon_state.write() {
            *s = daemon_state;
        }
        if let Ok(mut op) = ctx.current_operation.write() {
            *op = description;
        }
        Self {
            flag: Arc::clone(&ctx.operation_in_progress),
            state: Arc::clone(&ctx.daemon_state),
            operation: Arc::clone(&ctx.current_operation),
        }
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Relaxed);
        if let Ok(mut s) = self.state.write() {
            *s = fbuild_core::DaemonState::Idle;
        }
        if let Ok(mut op) = self.operation.write() {
            *op = None;
        }
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

    let _op_guard = OperationGuard::new(
        &ctx,
        fbuild_core::DaemonState::Building,
        Some(format!("Building {}", req.project_dir)),
    );

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
    // FBUILD_COMPILEDB env var: always-on by default (set to "0" to disable),
    // matching Python behavior. Explicit request flag also enables it.
    let compiledb_env = std::env::var("FBUILD_COMPILEDB")
        .map(|v| v != "0")
        .unwrap_or(true);
    let generate_compiledb = req.generate_compiledb || compiledb_env;
    let params = fbuild_build::BuildParams {
        project_dir: project_dir.clone(),
        env_name: env_name.clone(),
        clean: req.clean_build,
        profile,
        build_dir,
        verbose: req.verbose,
        jobs: req.jobs,
        generate_compiledb,
        compiledb_only: req.compiledb_only,
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

    let _op_guard = OperationGuard::new(
        &ctx,
        fbuild_core::DaemonState::Deploying,
        Some(format!("Deploying {}", req.project_dir)),
    );

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

    // Firmware ledger: check if source+flags are unchanged → skip build+deploy
    let deploy_port_for_ledger = req.port.clone();
    if !req.skip_build && !req.clean_build {
        if let Some(ref port) = deploy_port_for_ledger {
            let proj = project_dir.clone();
            let source_hash = tokio::task::spawn_blocking(move || {
                fbuild_deploy::firmware_ledger::compute_source_hash(&proj)
            })
            .await
            .unwrap_or_default();

            // Extract build flags from env config
            let build_flags: Vec<String> = env_config
                .get("build_flags")
                .map(|f| f.split_whitespace().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            let flags_hash = fbuild_deploy::firmware_ledger::compute_build_flags_hash(&build_flags);

            if !ctx
                .firmware_ledger
                .needs_redeploy(port, &source_hash, Some(&flags_hash))
            {
                tracing::info!("firmware ledger: skipping build+deploy for {}", port);
                return (
                    StatusCode::OK,
                    Json(OperationResponse {
                        success: true,
                        request_id,
                        message: "firmware unchanged, skipping build+deploy".to_string(),
                        exit_code: 0,
                        output_file: None,
                    }),
                );
            }
        }
    }

    // Build first unless skip_build
    let firmware_path = if req.skip_build {
        // Look for existing firmware using the standard search order
        // (profiles: release/quick, base env dir, legacy .pio/build)
        match fbuild_paths::find_firmware(&project_dir, &env_name, None) {
            Some(path) => path,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(OperationResponse::fail(
                        request_id,
                        "no firmware found; run build first or remove skip_build".to_string(),
                    )),
                );
            }
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
            generate_compiledb: false,
            compiledb_only: false,
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
    let board_id = env_config.get("board").cloned().unwrap_or_else(|| {
        fbuild_build::get_platform_support(platform)
            .map(|s| s.default_board_id().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
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
                // Load MCU config to get flash offsets and esptool defaults.
                let mcu_config = fbuild_build::esp32::mcu_config::get_mcu_config(&board_config.mcu)
                    .unwrap_or_else(|_| {
                        fbuild_build::esp32::mcu_config::get_mcu_config("esp32").unwrap()
                    });
                let esptool_params = fbuild_deploy::esp32::EsptoolParams {
                    flash_mode: board_config
                        .flash_mode
                        .as_deref()
                        .unwrap_or(mcu_config.default_flash_mode())
                        .to_string(),
                    flash_freq: mcu_config.default_flash_freq().to_string(),
                    default_baud: mcu_config.default_baud().to_string(),
                    before_reset: mcu_config.before_reset().to_string(),
                    after_reset: mcu_config.after_reset().to_string(),
                };
                Box::new(fbuild_deploy::esp32::Esp32Deployer::from_board_config(
                    &board_config,
                    mcu_config.bootloader_offset(),
                    mcu_config.partitions_offset(),
                    mcu_config.firmware_offset(),
                    &esptool_params,
                    false,
                ))
            }
            fbuild_core::Platform::AtmelAvr | fbuild_core::Platform::AtmelMegaAvr => {
                let board_config =
                    fbuild_config::BoardConfig::from_board_id(&board_id, &Default::default())
                        .unwrap_or_else(|_| {
                            fbuild_config::BoardConfig::from_board_id("uno", &Default::default())
                                .unwrap()
                        });
                let avr_config = fbuild_build::avr::mcu_config::get_avr_config().unwrap();
                let avrdude_params = fbuild_deploy::avr::AvrdudeParams {
                    default_programmer: avr_config.avrdude.default_programmer.clone(),
                    default_baud: avr_config.avrdude.default_baud.to_string(),
                    timeout_secs: avr_config.avrdude.timeout_secs,
                };
                Box::new(fbuild_deploy::avr::AvrDeployer::from_board_config(
                    &board_config,
                    &avrdude_params,
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

    let deploy_success = match deploy_result {
        Ok(Ok(r)) if r.success => true,
        Ok(Ok(r)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse {
                    success: false,
                    request_id,
                    message: r.message,
                    exit_code: 1,
                    output_file: Some(firmware_path.to_string_lossy().to_string()),
                }),
            );
        }
        Ok(Err(e)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("deploy error: {}", e),
                )),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("deploy task panicked: {}", e),
                )),
            );
        }
    };

    // Record successful deployment in firmware ledger
    if deploy_success {
        if let Some(ref port) = deploy_port_str {
            let fw = firmware_path.clone();
            let proj_dir = project_dir.to_string_lossy().to_string();
            let env = env_name.clone();
            let proj_for_hash = project_dir.clone();

            // Compute hashes in blocking context
            let ledger_result = tokio::task::spawn_blocking(move || {
                let fw_hash =
                    fbuild_deploy::firmware_ledger::compute_firmware_hash(&fw).unwrap_or_default();
                let src_hash = fbuild_deploy::firmware_ledger::compute_source_hash(&proj_for_hash);
                (fw_hash, src_hash)
            })
            .await;

            if let Ok((fw_hash, src_hash)) = ledger_result {
                let build_flags: Vec<String> = env_config
                    .get("build_flags")
                    .map(|f| f.split_whitespace().map(|s| s.to_string()).collect())
                    .unwrap_or_default();
                let flags_hash =
                    fbuild_deploy::firmware_ledger::compute_build_flags_hash(&build_flags);

                ctx.firmware_ledger.record_deployment(
                    port,
                    &fw_hash,
                    &src_hash,
                    &proj_dir,
                    &env,
                    Some(&flags_hash),
                );
            }
        }
    }

    // Post-deploy monitoring: if monitor_after is set, open the serial port
    // and stream lines checking halt conditions (matching Python behavior).
    if deploy_success && req.monitor_after {
        let monitor_port = deploy_port_str.unwrap_or_else(|| "/dev/ttyUSB0".to_string());
        let baud_rate = 115200u32;

        // Open the port for monitoring
        if let Err(e) = ctx
            .serial_manager
            .open_port(&monitor_port, baud_rate, &request_id)
            .await
        {
            return (
                StatusCode::OK,
                Json(OperationResponse {
                    success: true,
                    request_id,
                    message: format!("deploy succeeded but monitor failed to open port: {}", e),
                    exit_code: 0,
                    output_file: Some(firmware_path.to_string_lossy().to_string()),
                }),
            );
        }

        // Subscribe to broadcast channel
        let mut rx = match ctx.serial_manager.attach_reader(&monitor_port, &request_id) {
            Some(rx) => rx,
            None => {
                return (
                    StatusCode::OK,
                    Json(OperationResponse {
                        success: true,
                        request_id,
                        message: "deploy succeeded but monitor could not attach reader".to_string(),
                        exit_code: 0,
                        output_file: Some(firmware_path.to_string_lossy().to_string()),
                    }),
                );
            }
        };

        let monitor_result = run_monitor_loop(
            &mut rx,
            req.monitor_timeout,
            req.monitor_halt_on_error.as_deref(),
            req.monitor_halt_on_success.as_deref(),
            req.monitor_expect.as_deref(),
            req.monitor_show_timestamp,
        )
        .await;

        ctx.serial_manager.detach_reader(&monitor_port, &request_id);

        return match monitor_result {
            MonitorOutcome::Success(msg) => (
                StatusCode::OK,
                Json(OperationResponse {
                    success: true,
                    request_id,
                    message: format!("deploy succeeded; monitor: {}", msg),
                    exit_code: 0,
                    output_file: Some(firmware_path.to_string_lossy().to_string()),
                }),
            ),
            MonitorOutcome::Error(msg) => (
                StatusCode::OK,
                Json(OperationResponse {
                    success: false,
                    request_id,
                    message: format!("deploy succeeded; monitor error: {}", msg),
                    exit_code: 1,
                    output_file: Some(firmware_path.to_string_lossy().to_string()),
                }),
            ),
            MonitorOutcome::Timeout { expect_found } => {
                let (success, code) = if expect_found {
                    (true, 0)
                } else {
                    // If expect was set and not found, that's an error
                    (
                        req.monitor_expect.is_none(),
                        if req.monitor_expect.is_none() { 0 } else { 1 },
                    )
                };
                (
                    StatusCode::OK,
                    Json(OperationResponse {
                        success,
                        request_id,
                        message: format!(
                            "deploy succeeded; monitor timed out{}",
                            if !expect_found && req.monitor_expect.is_some() {
                                " (expected pattern not found)"
                            } else {
                                ""
                            }
                        ),
                        exit_code: code,
                        output_file: Some(firmware_path.to_string_lossy().to_string()),
                    }),
                )
            }
        };
    }

    (
        StatusCode::OK,
        Json(OperationResponse {
            success: true,
            request_id,
            message: "deploy succeeded".to_string(),
            exit_code: 0,
            output_file: Some(firmware_path.to_string_lossy().to_string()),
        }),
    )
}

/// Outcome of a post-deploy monitor session.
enum MonitorOutcome {
    /// halt-on-success pattern matched
    Success(String),
    /// halt-on-error pattern matched
    Error(String),
    /// Timeout reached
    Timeout { expect_found: bool },
}

/// Run a monitor loop reading lines from broadcast, checking halt conditions
/// using case-insensitive regex (matching Python's re.search behavior).
async fn run_monitor_loop(
    rx: &mut tokio::sync::broadcast::Receiver<String>,
    timeout_secs: Option<f64>,
    halt_on_error: Option<&str>,
    halt_on_success: Option<&str>,
    expect: Option<&str>,
    show_timestamp: bool,
) -> MonitorOutcome {
    let halt_error_re = halt_on_error.and_then(|p| {
        regex::RegexBuilder::new(p)
            .case_insensitive(true)
            .build()
            .ok()
    });
    let halt_success_re = halt_on_success.and_then(|p| {
        regex::RegexBuilder::new(p)
            .case_insensitive(true)
            .build()
            .ok()
    });
    let expect_re = expect.and_then(|p| {
        regex::RegexBuilder::new(p)
            .case_insensitive(true)
            .build()
            .ok()
    });

    let start = std::time::Instant::now();
    let timeout_dur = timeout_secs.map(std::time::Duration::from_secs_f64);
    let mut expect_found = false;

    loop {
        // Check timeout
        if let Some(dur) = timeout_dur {
            if start.elapsed() >= dur {
                return MonitorOutcome::Timeout { expect_found };
            }
        }

        let remaining = timeout_dur.map(|dur| dur.saturating_sub(start.elapsed()));
        let recv_timeout = remaining.unwrap_or(std::time::Duration::from_secs(1));

        match tokio::time::timeout(recv_timeout, rx.recv()).await {
            Ok(Ok(line)) => {
                // Print line (with optional timestamp prefix in MM:SS.cc format)
                if show_timestamp {
                    let total_secs = start.elapsed().as_secs_f64();
                    let minutes = (total_secs / 60.0) as u64;
                    let seconds = total_secs % 60.0;
                    tracing::info!("{:02}:{:05.2} {}", minutes, seconds, line);
                } else {
                    tracing::info!("{}", line);
                }

                // Check expect pattern
                if let Some(ref re) = expect_re {
                    if re.is_match(&line) {
                        expect_found = true;
                    }
                }

                // Check halt-on-error
                if let Some(ref re) = halt_error_re {
                    if re.is_match(&line) {
                        return MonitorOutcome::Error(format!(
                            "halt-on-error pattern matched: {}",
                            line
                        ));
                    }
                }

                // Check halt-on-success
                if let Some(ref re) = halt_success_re {
                    if re.is_match(&line) {
                        return MonitorOutcome::Success(format!(
                            "halt-on-success pattern matched: {}",
                            line
                        ));
                    }
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                tracing::warn!("monitor lagged, skipped {} messages", n);
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return MonitorOutcome::Timeout { expect_found };
            }
            Err(_) => {
                // Timeout on recv — check if overall timeout expired
                if let Some(dur) = timeout_dur {
                    if start.elapsed() >= dur {
                        return MonitorOutcome::Timeout { expect_found };
                    }
                }
                // No overall timeout: just keep waiting
            }
        }
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

    if let Err(e) = ctx
        .serial_manager
        .open_port(&port, baud_rate, &request_id)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("failed to open port: {}", e),
            )),
        );
    }

    // If halt conditions or timeout are set, run a monitor loop
    let has_conditions = req.halt_on_error.is_some()
        || req.halt_on_success.is_some()
        || req.expect.is_some()
        || req.timeout.is_some();

    if has_conditions {
        let mut rx = match ctx.serial_manager.attach_reader(&port, &request_id) {
            Some(rx) => rx,
            None => {
                return (
                    StatusCode::OK,
                    Json(OperationResponse::ok(
                        request_id,
                        format!(
                            "monitoring {} at {} baud (no broadcast channel)",
                            port, baud_rate
                        ),
                    )),
                );
            }
        };

        let result = run_monitor_loop(
            &mut rx,
            req.timeout,
            req.halt_on_error.as_deref(),
            req.halt_on_success.as_deref(),
            req.expect.as_deref(),
            req.show_timestamp,
        )
        .await;

        ctx.serial_manager.detach_reader(&port, &request_id);

        return match result {
            MonitorOutcome::Success(msg) => {
                (StatusCode::OK, Json(OperationResponse::ok(request_id, msg)))
            }
            MonitorOutcome::Error(msg) => (
                StatusCode::OK,
                Json(OperationResponse::fail(request_id, msg)),
            ),
            MonitorOutcome::Timeout { expect_found } => {
                if req.expect.is_some() && !expect_found {
                    (
                        StatusCode::OK,
                        Json(OperationResponse::fail(
                            request_id,
                            "monitor timed out (expected pattern not found)".to_string(),
                        )),
                    )
                } else {
                    (
                        StatusCode::OK,
                        Json(OperationResponse::ok(
                            request_id,
                            "monitor completed (timeout)".to_string(),
                        )),
                    )
                }
            }
        };
    }

    (
        StatusCode::OK,
        Json(OperationResponse::ok(
            request_id,
            format!("monitoring {} at {} baud", port, baud_rate),
        )),
    )
}

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
    let result = tokio::task::spawn_blocking(move || {
        fbuild_deploy::reset::reset_device(&platform_str, &port, verbose)
    })
    .await;

    // Clear preemption
    ctx.serial_manager.clear_preemption(&req.port).await;

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
    let result = tokio::task::spawn_blocking(move || {
        fbuild_build::install_platform_deps(platform, &project_dir)
    })
    .await;

    match result {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(OperationResponse::ok(
                request_id,
                format!("Dependencies installed for environment '{}'", env_label),
            )),
        ),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("install-deps error: {}", e),
            )),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("install-deps task panicked: {}", e),
            )),
        ),
    }
}
