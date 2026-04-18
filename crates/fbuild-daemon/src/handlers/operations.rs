//! Build, deploy, and monitor operation handlers.

use crate::context::DaemonContext;
use crate::models::{
    BuildRequest, DeployRequest, InstallDepsRequest, MonitorRequest, OperationResponse,
    ResetRequest,
};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Returns `true` when the daemon should route ESP32 `verify-flash`
/// pre-checks through the native [`espflash`] crate (issue #66) instead
/// of the Python `esptool` subprocess.
///
/// Controlled by the `FBUILD_USE_ESPFLASH_VERIFY` environment variable
/// (set to `1`, `true`, `yes`, or `on` to enable — case-insensitive).
/// Any other value — including unset — keeps the default esptool path,
/// so users on unusual setups retain the existing escape hatch until
/// the native path has bench time on every ESP32 family member.
pub(crate) fn native_verify_enabled() -> bool {
    match std::env::var("FBUILD_USE_ESPFLASH_VERIFY") {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

pub(crate) fn qemu_extra_build_flags(platform: fbuild_core::Platform, mcu: &str) -> Vec<String> {
    if platform == fbuild_core::Platform::Espressif32 && mcu.eq_ignore_ascii_case("esp32s3") {
        vec![
            "-DARDUINO_USB_MODE=0".to_string(),
            "-DARDUINO_USB_CDC_ON_BOOT=0".to_string(),
        ]
    } else {
        Vec::new()
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmulatorKind {
    Qemu,
    Avr8js,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeployRoute {
    Device,
    Emulator(EmulatorKind),
}

fn parse_emulator_kind(raw: &str) -> fbuild_core::Result<EmulatorKind> {
    match raw {
        "qemu" => Ok(EmulatorKind::Qemu),
        "avr8js" => Ok(EmulatorKind::Avr8js),
        other => Err(fbuild_core::FbuildError::DeployFailed(format!(
            "unsupported emulator '{}'",
            other
        ))),
    }
}

fn infer_default_emulator_kind(platform: fbuild_core::Platform, mcu: &str) -> Option<EmulatorKind> {
    match platform {
        fbuild_core::Platform::AtmelAvr | fbuild_core::Platform::AtmelMegaAvr => {
            Some(EmulatorKind::Avr8js)
        }
        fbuild_core::Platform::Espressif32 if mcu.eq_ignore_ascii_case("esp32s3") => {
            Some(EmulatorKind::Qemu)
        }
        _ => None,
    }
}

fn parse_deploy_route(
    req: &DeployRequest,
    default_emulator: Option<EmulatorKind>,
) -> fbuild_core::Result<DeployRoute> {
    if let Some(target) = req.target.as_deref() {
        return match target {
            "device" => Ok(DeployRoute::Device),
            "qemu" => Ok(DeployRoute::Emulator(EmulatorKind::Qemu)),
            "avr8js" => Ok(DeployRoute::Emulator(EmulatorKind::Avr8js)),
            other => Err(fbuild_core::FbuildError::DeployFailed(format!(
                "unsupported deploy target '{}'",
                other
            ))),
        };
    }

    let destination = req.to.as_deref().unwrap_or("device");
    match destination {
        "device" => {
            if req.qemu {
                return Err(fbuild_core::FbuildError::DeployFailed(
                    "--qemu cannot be combined with --to device".to_string(),
                ));
            }
            if let Some(emulator) = req.emulator.as_deref() {
                return Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "--emulator {} requires --to emu",
                    emulator
                )));
            }
            Ok(DeployRoute::Device)
        }
        "emu" | "emulator" => {
            let emulator = if req.qemu {
                if let Some(explicit) = req.emulator.as_deref() {
                    if explicit != "qemu" {
                        return Err(fbuild_core::FbuildError::DeployFailed(
                            "--qemu cannot be combined with a different --emulator".to_string(),
                        ));
                    }
                }
                "qemu"
            } else {
                match req.emulator.as_deref() {
                    Some(explicit) => explicit,
                    None => match default_emulator {
                        Some(EmulatorKind::Qemu) => "qemu",
                        Some(EmulatorKind::Avr8js) => "avr8js",
                        None => {
                            return Err(fbuild_core::FbuildError::DeployFailed(
                                "--to emu requires an explicit --emulator for this board"
                                    .to_string(),
                            ))
                        }
                    },
                }
            };
            Ok(DeployRoute::Emulator(parse_emulator_kind(emulator)?))
        }
        other => Err(fbuild_core::FbuildError::DeployFailed(format!(
            "unsupported deploy destination '{}'",
            other
        ))),
    }
}

fn resolve_client_path(raw: &str, caller_cwd: Option<&str>, project_dir: &Path) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else if let Some(cwd) = caller_cwd {
        PathBuf::from(cwd).join(path)
    } else {
        project_dir.join(path)
    }
}

#[derive(Debug, Serialize)]
struct ArtifactFileEntry {
    name: String,
    role: String,
}

#[derive(Debug, Serialize)]
struct ArtifactManifest {
    platform: String,
    environment: String,
    primary_firmware: Option<String>,
    elf: Option<String>,
    files: Vec<ArtifactFileEntry>,
}

struct ArtifactExportResult {
    output_dir: PathBuf,
    primary_output: Option<PathBuf>,
}

fn artifact_role(name: &str, primary_firmware: Option<&Path>, elf_path: Option<&Path>) -> String {
    if primary_firmware
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == name)
    {
        "firmware".to_string()
    } else if elf_path
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == name)
    {
        "elf".to_string()
    } else {
        match name {
            "bootloader.bin" => "bootloader".to_string(),
            "partitions.bin" => "partitions".to_string(),
            "compile_commands.json" => "compile_database".to_string(),
            "symbol_analysis.txt" => "symbol_analysis".to_string(),
            _ => "artifact".to_string(),
        }
    }
}

fn export_artifacts_bundle(
    output_dir: &Path,
    platform: fbuild_core::Platform,
    env_name: &str,
    primary_firmware: Option<&Path>,
    elf_path: Option<&Path>,
) -> fbuild_core::Result<ArtifactExportResult> {
    std::fs::create_dir_all(output_dir)?;

    let source_dir = primary_firmware
        .and_then(|p| p.parent())
        .or_else(|| elf_path.and_then(|p| p.parent()))
        .ok_or_else(|| {
            fbuild_core::FbuildError::Other(
                "could not determine source artifact directory for export".to_string(),
            )
        })?;

    let mut copied_names = Vec::new();
    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = match path.file_name() {
            Some(name) => name,
            None => continue,
        };
        let dest = output_dir.join(file_name);
        if path != dest {
            std::fs::copy(&path, &dest)?;
        }
        copied_names.push(file_name.to_string_lossy().to_string());
    }
    copied_names.sort();
    copied_names.dedup();

    let manifest = ArtifactManifest {
        platform: format!("{:?}", platform),
        environment: env_name.to_string(),
        primary_firmware: primary_firmware
            .and_then(|p| p.file_name())
            .map(|p| p.to_string_lossy().to_string()),
        elf: elf_path
            .and_then(|p| p.file_name())
            .map(|p| p.to_string_lossy().to_string()),
        files: copied_names
            .iter()
            .map(|name| ArtifactFileEntry {
                name: name.clone(),
                role: artifact_role(name, primary_firmware, elf_path),
            })
            .collect(),
    };

    std::fs::write(
        output_dir.join("artifacts.json"),
        serde_json::to_vec_pretty(&manifest).map_err(|e| {
            fbuild_core::FbuildError::Other(format!("failed to serialize artifact manifest: {}", e))
        })?,
    )?;

    let primary_output = primary_firmware
        .and_then(|p| p.file_name())
        .map(|name| output_dir.join(name));

    Ok(ArtifactExportResult {
        output_dir: output_dir.to_path_buf(),
        primary_output,
    })
}

/// POST /api/build
pub async fn build(
    State(ctx): State<Arc<DaemonContext>>,
    Json(req): Json<BuildRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let request_id = req
        .request_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let project_dir = PathBuf::from(&req.project_dir);
    let stream = req.stream;

    if !project_dir.exists() {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!("project directory does not exist: {}", req.project_dir),
            )),
        )
            .into_response();
    }

    // Validation: parse config, resolve platform
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
                )
                    .into_response();
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
            )
                .into_response();
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
            )
                .into_response();
        }
    };

    let profile = match req.profile.as_deref() {
        Some("quick") => fbuild_core::BuildProfile::Quick,
        _ => fbuild_core::BuildProfile::Release,
    };

    let build_dir = fbuild_paths::get_project_build_root(&project_dir);
    let compiledb_env = std::env::var("FBUILD_COMPILEDB")
        .map(|v| v != "0")
        .unwrap_or(true);
    let generate_compiledb = req.generate_compiledb || compiledb_env;
    let resolved_symbol_analysis_path = req
        .symbol_analysis_path
        .as_deref()
        .map(|p| resolve_client_path(p, req.caller_cwd.as_deref(), &project_dir));
    let resolved_output_dir = req
        .output_dir
        .as_deref()
        .map(|p| resolve_client_path(p, req.caller_cwd.as_deref(), &project_dir));

    if stream {
        // --- STREAMING PATH ---
        // Build runs in a background task; log lines stream to client as NDJSON.
        let (sync_tx, sync_rx) = std::sync::mpsc::channel::<String>();
        let (async_tx, async_rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();

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
            log_sender: Some(sync_tx),
            symbol_analysis: req.symbol_analysis,
            symbol_analysis_path: resolved_symbol_analysis_path.clone(),
            no_timestamp: req.no_timestamp,
            src_dir: req.src_dir.clone(),
            pio_env: req.pio_env.clone(),
            extra_build_flags: Vec::new(),
        };

        let project_dir_desc = req.project_dir.clone();
        tokio::spawn(async move {
            // FBUILD_PERF_LOG=1 enables daemon-side coarse phase timing
            // (lock-wait + build). Zero overhead when unset.
            let perf_enabled = std::env::var("FBUILD_PERF_LOG")
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false);
            let daemon_start = std::time::Instant::now();

            let _op_guard = OperationGuard::new(
                &ctx,
                fbuild_core::DaemonState::Building,
                Some(format!("Building {}", project_dir_desc)),
            );
            let lock_wait_start = std::time::Instant::now();
            let lock = ctx.project_lock(&project_dir);
            let _lock_guard = lock.lock().await;
            let lock_wait = lock_wait_start.elapsed();

            // Bridge: sync log lines → async NDJSON chunks
            let bridge_tx = async_tx.clone();
            let bridge = tokio::task::spawn_blocking(move || {
                for line in sync_rx {
                    let event = serde_json::json!({"type": "log", "message": line});
                    let mut chunk = event.to_string();
                    chunk.push('\n');
                    if bridge_tx.send(bytes::Bytes::from(chunk)).is_err() {
                        break;
                    }
                }
            });

            // Run build
            let build_wallclock_start = std::time::Instant::now();
            let build_result = tokio::task::spawn_blocking(move || {
                let orchestrator = fbuild_build::get_orchestrator(platform)?;
                orchestrator.build(&params)
            })
            .await;
            let build_wallclock = build_wallclock_start.elapsed();
            if perf_enabled {
                let summary = format!(
                    "[perf-log daemon-handler] lock-wait={} ms, build-wallclock={} ms, total={} ms",
                    lock_wait.as_millis(),
                    build_wallclock.as_millis(),
                    daemon_start.elapsed().as_millis(),
                );
                tracing::info!(target: "fbuild_daemon::perf_log", "{}", summary);
                eprintln!("{}", summary);
            }

            // Extract result (drops BuildLog sender so bridge can finish)
            let (success, rid, msg, code, output_file, output_dir) = match build_result {
                Ok(Ok(br)) => {
                    let exported = if br.success {
                        if let Some(ref out_dir) = resolved_output_dir {
                            Some(export_artifacts_bundle(
                                out_dir,
                                platform,
                                &env_name,
                                br.firmware_path.as_deref(),
                                br.elf_path.as_deref(),
                            ))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let _lines = br.build_log.into_lines(); // drop sender
                    let summary = if br.success {
                        let size_str = br
                            .size_info
                            .as_ref()
                            .map(|s| {
                                format!(
                                    " (flash: {} bytes, ram: {} bytes)",
                                    s.total_flash, s.total_ram
                                )
                            })
                            .unwrap_or_default();
                        let export_suffix = match exported.as_ref() {
                            Some(Ok(result)) => {
                                format!("; artifacts exported to {}", result.output_dir.display())
                            }
                            Some(Err(e)) => {
                                format!("; artifact export failed: {}", e)
                            }
                            None => String::new(),
                        };
                        format!(
                            "build succeeded in {:.1}s{}{}",
                            br.build_time_secs, size_str, export_suffix
                        )
                    } else {
                        br.message.clone()
                    };
                    let output = match exported.as_ref() {
                        Some(Ok(result)) => result
                            .primary_output
                            .clone()
                            .or(br.firmware_path.clone())
                            .or(br.elf_path.clone()),
                        _ => br.firmware_path.clone().or(br.elf_path.clone()),
                    }
                    .map(|p| p.to_string_lossy().to_string());
                    let output_dir = match exported.as_ref() {
                        Some(Ok(result)) => Some(result.output_dir.to_string_lossy().to_string()),
                        _ => None,
                    };
                    let c = if br.success { 0 } else { 1 };
                    (
                        br.success,
                        request_id.clone(),
                        summary,
                        c,
                        output,
                        output_dir,
                    )
                }
                Ok(Err(e)) => (
                    false,
                    request_id.clone(),
                    format!("build error: {}", e),
                    1,
                    None,
                    None,
                ),
                Err(e) => (
                    false,
                    request_id.clone(),
                    format!("build task panicked: {}", e),
                    1,
                    None,
                    None,
                ),
            };

            let _ = bridge.await;

            let result_event = serde_json::json!({
                "type": "result",
                "success": success,
                "request_id": rid,
                "message": msg,
                "exit_code": code,
                "output_file": output_file,
                "output_dir": output_dir,
            });
            let mut chunk = result_event.to_string();
            chunk.push('\n');
            let _ = async_tx.send(bytes::Bytes::from(chunk));
        });

        // Return streaming response immediately
        let stream = futures::stream::unfold(async_rx, |mut rx| async move {
            rx.recv()
                .await
                .map(|data| (Ok::<_, std::convert::Infallible>(data), rx))
        });
        let body = axum::body::Body::from_stream(stream);
        axum::response::Response::builder()
            .header("content-type", "application/x-ndjson")
            .body(body)
            .unwrap()
            .into_response()
    } else {
        // --- NON-STREAMING PATH (existing behavior) ---
        let _op_guard = OperationGuard::new(
            &ctx,
            fbuild_core::DaemonState::Building,
            Some(format!("Building {}", req.project_dir)),
        );
        let lock = ctx.project_lock(&project_dir);
        let _guard = lock.lock().await;

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
            log_sender: None,
            symbol_analysis: req.symbol_analysis,
            symbol_analysis_path: resolved_symbol_analysis_path,
            no_timestamp: req.no_timestamp,
            src_dir: req.src_dir,
            pio_env: req.pio_env,
            extra_build_flags: Vec::new(),
        };

        let result = tokio::task::spawn_blocking(move || {
            let orchestrator = fbuild_build::get_orchestrator(platform)?;
            orchestrator.build(&params)
        })
        .await;

        match result {
            Ok(Ok(build_result)) => {
                let exported = if build_result.success {
                    if let Some(ref out_dir) = resolved_output_dir {
                        Some(export_artifacts_bundle(
                            out_dir,
                            platform,
                            &env_name,
                            build_result.firmware_path.as_deref(),
                            build_result.elf_path.as_deref(),
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                };
                let summary = if build_result.success {
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
                    let export_suffix = match exported.as_ref() {
                        Some(Ok(result)) => {
                            format!("; artifacts exported to {}", result.output_dir.display())
                        }
                        Some(Err(e)) => format!("; artifact export failed: {}", e),
                        None => String::new(),
                    };
                    format!(
                        "build succeeded in {:.1}s{}{}",
                        build_result.build_time_secs, size_str, export_suffix
                    )
                } else {
                    build_result.message.clone()
                };
                let msg = if build_result.build_log.is_empty() {
                    summary
                } else {
                    let mut lines = build_result.build_log.into_lines();
                    lines.push(summary);
                    lines.join("\n")
                };
                let output_file = match exported.as_ref() {
                    Some(Ok(result)) => result
                        .primary_output
                        .clone()
                        .or(build_result.firmware_path.clone())
                        .or(build_result.elf_path.clone()),
                    _ => build_result
                        .firmware_path
                        .clone()
                        .or(build_result.elf_path.clone()),
                }
                .map(|p| p.to_string_lossy().to_string());
                let output_dir = match exported.as_ref() {
                    Some(Ok(result)) => Some(result.output_dir.to_string_lossy().to_string()),
                    _ => None,
                };
                let code = if build_result.success { 0 } else { 1 };
                (
                    StatusCode::OK,
                    Json(OperationResponse {
                        success: build_result.success,
                        request_id,
                        message: msg,
                        exit_code: code,
                        output_file,
                        output_dir,
                        launch_url: None,
                        stdout: None,
                        stderr: None,
                    }),
                )
                    .into_response()
            }
            Ok(Err(e)) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("build error: {}", e),
                )),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("build task panicked: {}", e),
                )),
            )
                .into_response(),
        }
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
    let resolved_output_dir = req
        .output_dir
        .as_deref()
        .map(|p| resolve_client_path(p, req.caller_cwd.as_deref(), &project_dir));

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
    let board_id = env_config.get("board").cloned().unwrap_or_else(|| {
        fbuild_build::get_platform_support(platform)
            .map(|s| s.default_board_id().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    });
    let board_overrides = config.get_board_overrides(&env_name).unwrap_or_default();
    let board = fbuild_config::BoardConfig::from_board_id(&board_id, &board_overrides)
        .or_else(|_| fbuild_config::BoardConfig::from_board_id(&board_id, &HashMap::new()))
        .ok();
    let deploy_route = match parse_deploy_route(
        &req,
        board
            .as_ref()
            .and_then(|board| infer_default_emulator_kind(platform, &board.mcu)),
    ) {
        Ok(route) => route,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(request_id, e.to_string())),
            );
        }
    };

    // Build first unless skip_build
    let (firmware_path, elf_path) = if req.skip_build {
        // Look for existing firmware using the standard search order
        // (profiles: release/quick, base env dir, legacy .pio/build)
        match fbuild_paths::find_firmware(&project_dir, &env_name, None) {
            Some(path) => {
                let elf = path.parent().map(|dir| dir.join("firmware.elf"));
                let elf = elf.filter(|p| p.exists());
                (path, elf)
            }
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
            log_sender: None,
            symbol_analysis: false,
            symbol_analysis_path: None,
            no_timestamp: false,
            src_dir: req.src_dir,
            pio_env: req.pio_env,
            extra_build_flags: if deploy_route == DeployRoute::Emulator(EmulatorKind::Qemu) {
                board
                    .as_ref()
                    .map(|board| qemu_extra_build_flags(platform, &board.mcu))
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
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
            Ok(Ok(r)) if r.success => {
                let fw = r.firmware_path.clone().unwrap_or_else(|| {
                    r.elf_path
                        .clone()
                        .unwrap_or_else(|| PathBuf::from("firmware.bin"))
                });
                (fw, r.elf_path)
            }
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

    let artifact_export = match resolved_output_dir.as_ref() {
        Some(out_dir) => match export_artifacts_bundle(
            out_dir,
            platform,
            &env_name,
            Some(&firmware_path),
            elf_path.as_deref(),
        ) {
            Ok(result) => Some(result),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("failed to export artifacts: {}", e),
                    )),
                );
            }
        },
        None => None,
    };

    let reported_output_file = artifact_export
        .as_ref()
        .and_then(|r| r.primary_output.clone())
        .unwrap_or_else(|| firmware_path.clone())
        .to_string_lossy()
        .to_string();
    let reported_output_dir = artifact_export
        .as_ref()
        .map(|r| r.output_dir.to_string_lossy().to_string());

    if deploy_route == DeployRoute::Emulator(EmulatorKind::Avr8js) {
        return crate::handlers::emulator::deploy_avr8js(
            ctx,
            crate::handlers::emulator::DeployAvr8jsRequest {
                request_id,
                project_dir,
                env_name,
                board_id,
                platform,
                firmware_path,
                elf_path,
                monitor_after: req.monitor_after,
                output_file: reported_output_file,
                output_dir: reported_output_dir,
                monitor_timeout: req.monitor_timeout,
                halt_on_error: req.monitor_halt_on_error.clone(),
                halt_on_success: req.monitor_halt_on_success.clone(),
                expect: req.monitor_expect.clone(),
                show_timestamp: req.monitor_show_timestamp,
                verbose: req.verbose,
            },
        )
        .await;
    }

    if deploy_route == DeployRoute::Emulator(EmulatorKind::Qemu) {
        return crate::handlers::emulator::deploy_qemu(
            ctx,
            crate::handlers::emulator::DeployQemuRequest {
                request_id,
                project_dir,
                env_name,
                board_id,
                platform,
                firmware_path,
                elf_path,
                output_file: reported_output_file,
                output_dir: reported_output_dir,
                monitor_timeout: req.monitor_timeout,
                qemu_timeout_secs: req.qemu_timeout,
                halt_on_error: req.monitor_halt_on_error.clone(),
                halt_on_success: req.monitor_halt_on_success.clone(),
                expect: req.monitor_expect.clone(),
                show_timestamp: req.monitor_show_timestamp,
                verbose: req.verbose,
                board_overrides,
            },
        )
        .await;
    }

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

    // Extract env-section board_build.* / board_upload.* overrides BEFORE
    // spawn_blocking (env_config borrows config). Without these, fbuild
    // would silently ignore `board_build.flash_mode = dio` (and friends)
    // from the user's [env:X] section and fall back to whatever the board
    // JSON says — producing a firmware/bootloader that doesn't match the
    // hardware. The build phase already passes overrides; the deploy phase
    // must do the same so esptool flashes with the right --flash-mode.
    let board_overrides = config.get_board_overrides(&env_name).unwrap_or_default();

    // Deploy
    let deploy_env = env_name.clone();
    let deploy_project = project_dir.clone();
    let deploy_port = deploy_port_str.clone();
    let deploy_fw = firmware_path.clone();
    let baud_override = req.baud_rate;
    let deploy_board_overrides = board_overrides.clone();
    let deploy_result = tokio::task::spawn_blocking(move || {
        let deployer: Box<dyn fbuild_deploy::Deployer> = match platform {
            fbuild_core::Platform::Espressif32 => {
                let board_config =
                    fbuild_config::BoardConfig::from_board_id(&board_id, &deploy_board_overrides)
                        .unwrap_or_else(|_| {
                            fbuild_config::BoardConfig::from_board_id(
                                "esp32dev",
                                &deploy_board_overrides,
                            )
                            .unwrap()
                        });
                // Load MCU config to get flash offsets and esptool defaults.
                let mcu_config = fbuild_build::esp32::mcu_config::get_mcu_config(&board_config.mcu)
                    .unwrap_or_else(|_| {
                        fbuild_build::esp32::mcu_config::get_mcu_config("esp32").unwrap()
                    });
                // Flash mode: `board_config.flash_mode` is `None` for ESP32
                // chips unless the user explicitly set `board_build.flash_mode`
                // in their `[env:X]` section (see `BoardConfig::from_board_id`
                // — the JSON-shipped value is intentionally dropped for ESP32
                // because ESP32-S3's QIE-bit init is unreliable). The unwrap
                // therefore falls back to the per-MCU default "dio".
                let esptool_params = fbuild_deploy::esp32::EsptoolParams {
                    flash_mode: board_config
                        .flash_mode
                        .as_deref()
                        .unwrap_or(mcu_config.default_flash_mode())
                        .to_string(),
                    flash_freq: {
                        let f_for_image = board_config
                            .f_image
                            .as_deref()
                            .or(board_config.f_flash.as_deref());
                        fbuild_build::esp32::esp32_linker::f_flash_to_esptool_freq(
                            f_for_image,
                            mcu_config.default_flash_freq(),
                        )
                    },
                    default_baud: mcu_config.default_baud().to_string(),
                    before_reset: mcu_config.before_reset().to_string(),
                    after_reset: mcu_config.after_reset().to_string(),
                };
                let deployer = fbuild_deploy::esp32::Esp32Deployer::from_board_config(
                    &board_config,
                    mcu_config.bootloader_offset(),
                    mcu_config.partitions_offset(),
                    mcu_config.firmware_offset(),
                    &esptool_params,
                    false,
                );
                let deployer = if let Some(baud) = baud_override {
                    deployer.with_baud_rate(&baud.to_string())
                } else {
                    deployer
                };
                // Issue #66: opt-in native `verify-flash` via the
                // `espflash` crate. Off by default so esptool remains
                // the fallback path; set `FBUILD_USE_ESPFLASH_VERIFY=1`
                // to route the verify pre-check (not write-flash)
                // through espflash and skip the ~1.5 s Python subprocess
                // cost.
                let deployer = deployer.with_native_verify(native_verify_enabled());

                // Fast deploy: ask the device whether it already holds
                // the exact firmware/bootloader/partitions we'd be about
                // to write. Uses esptool's `verify-flash` which dispatches
                // to the stub flasher's `FLASH_MD5SUM` command — no full
                // read-back, just one MD5 round-trip per region.
                //
                // Measured on a 2.4 MB FastLED esp32s3 image:
                //   * fresh write-flash: ~25 s
                //   * verify-flash skip: ~6 s   (-19 s, ~76% faster)
                //
                // Falls through to the normal flash path silently on
                // mismatch or transport error so we never break a deploy
                // that the verify call didn't understand.
                let mut selective_regions: Option<Vec<fbuild_deploy::esp32::FlashRegion>> = None;
                if let Some(port) = deploy_port.as_deref() {
                    match deployer.try_verify_deployment(&deploy_fw, port) {
                        Ok(fbuild_deploy::esp32::VerifyOutcome::Match { stdout, stderr }) => {
                            tracing::info!(
                                port,
                                "verify-flash: device already running this exact image; skipping write"
                            );
                            return Ok(fbuild_deploy::DeploymentResult {
                                success: true,
                                message: format!(
                                    "firmware already current on {} (skipped via verify-flash)",
                                    port
                                ),
                                port: Some(port.to_string()),
                                stdout,
                                stderr,
                                outcome: fbuild_deploy::DeployOutcome::VerifySkip,
                            });
                        }
                        Ok(fbuild_deploy::esp32::VerifyOutcome::Mismatch { regions, .. }) => {
                            // Pick only the regions that actually differ
                            // so we avoid the ~1s bootloader/partitions
                            // rewrite when only firmware changed. Empty
                            // `regions` means parsing failed — fall back
                            // to full flash.
                            let to_write: Vec<_> = regions
                                .iter()
                                .filter(|r| !r.matched)
                                .map(|r| r.region)
                                .collect();
                            if !regions.is_empty() && !to_write.is_empty() && to_write.len() < 3 {
                                tracing::info!(
                                    port,
                                    "verify-flash: only {} region(s) differ; flashing selectively",
                                    to_write.len()
                                );
                                selective_regions = Some(to_write);
                            } else {
                                tracing::info!(
                                    port,
                                    "verify-flash: device image differs; proceeding with full flash"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                port,
                                "verify-flash pre-check failed ({}); proceeding with full flash",
                                e
                            );
                        }
                    }
                }

                if let (Some(regions), Some(port)) = (selective_regions, deploy_port.as_deref()) {
                    return deployer.deploy_regions(&deploy_fw, port, &regions);
                }

                Box::new(deployer)
            }
            fbuild_core::Platform::AtmelAvr | fbuild_core::Platform::AtmelMegaAvr => {
                let board_config =
                    fbuild_config::BoardConfig::from_board_id(&board_id, &deploy_board_overrides)
                        .unwrap_or_else(|_| {
                            fbuild_config::BoardConfig::from_board_id(
                                "uno",
                                &deploy_board_overrides,
                            )
                            .unwrap()
                        });
                let avr_config = fbuild_build::avr::mcu_config::get_avr_config().unwrap();
                let avrdude_params = fbuild_deploy::avr::AvrdudeParams {
                    default_programmer: avr_config.avrdude.default_programmer.clone(),
                    default_baud: avr_config.avrdude.default_baud.to_string(),
                    timeout_secs: avr_config.avrdude.timeout_secs,
                };
                let deployer = fbuild_deploy::avr::AvrDeployer::from_board_config(
                    &board_config,
                    &avrdude_params,
                    false,
                );
                let deployer = if let Some(baud) = baud_override {
                    deployer.with_baud_rate(&baud.to_string())
                } else {
                    deployer
                };
                Box::new(deployer)
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

    // Clear preemption then wait for USB re-enumeration.
    // Fast-poll the serial port instead of a hard 2s sleep — most ESP32-S3
    // boards with native USB re-enumerate in <500ms.
    if let Some(ref p) = deploy_port_str {
        ctx.serial_manager.clear_preemption(p).await;
        let port_name = p.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while std::time::Instant::now() < deadline {
                if serialport::new(&port_name, 115200)
                    .timeout(std::time::Duration::from_millis(50))
                    .open()
                    .is_ok()
                {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            tracing::warn!(
                "USB re-enumeration: port {} not available after 3s",
                port_name
            );
        })
        .await;
    }

    let (deploy_success, deploy_stdout, deploy_stderr, deploy_outcome) = match deploy_result {
        Ok(Ok(r)) if r.success => (true, Some(r.stdout), Some(r.stderr), r.outcome),
        Ok(Ok(r)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse {
                    success: false,
                    request_id,
                    message: r.message,
                    exit_code: 1,
                    output_file: Some(reported_output_file.clone()),
                    output_dir: reported_output_dir.clone(),
                    launch_url: None,
                    stdout: Some(r.stdout),
                    stderr: Some(r.stderr),
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
    // Build the "deploy succeeded (...)" prefix used by every
    // monitor-attached and non-monitor-attached response below. Stable
    // wording — see GitHub issue #76 and the DeployOutcome::describe
    // test in fbuild-deploy.
    let deploy_prefix = format!("deploy succeeded ({})", deploy_outcome.describe());

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
                    message: format!("{} but monitor failed to open port: {}", deploy_prefix, e),
                    exit_code: 0,
                    output_file: Some(reported_output_file.clone()),
                    output_dir: reported_output_dir.clone(),
                    launch_url: None,
                    stdout: deploy_stdout,
                    stderr: deploy_stderr,
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
                        message: format!("{} but monitor could not attach reader", deploy_prefix),
                        exit_code: 0,
                        output_file: Some(reported_output_file.clone()),
                        output_dir: reported_output_dir.clone(),
                        launch_url: None,
                        stdout: deploy_stdout,
                        stderr: deploy_stderr,
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
                    message: format!("{}; monitor: {}", deploy_prefix, msg),
                    exit_code: 0,
                    output_file: Some(reported_output_file.clone()),
                    output_dir: reported_output_dir.clone(),
                    launch_url: None,
                    stdout: deploy_stdout,
                    stderr: deploy_stderr,
                }),
            ),
            MonitorOutcome::Error(msg) => (
                StatusCode::OK,
                Json(OperationResponse {
                    success: false,
                    request_id,
                    message: format!("{}; monitor error: {}", deploy_prefix, msg),
                    exit_code: 1,
                    output_file: Some(reported_output_file.clone()),
                    output_dir: reported_output_dir.clone(),
                    launch_url: None,
                    stdout: deploy_stdout,
                    stderr: deploy_stderr,
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
                            "{}; monitor timed out{}",
                            deploy_prefix,
                            if !expect_found && req.monitor_expect.is_some() {
                                " (expected pattern not found)"
                            } else {
                                ""
                            }
                        ),
                        exit_code: code,
                        output_file: Some(reported_output_file.clone()),
                        output_dir: reported_output_dir.clone(),
                        launch_url: None,
                        stdout: deploy_stdout,
                        stderr: deploy_stderr,
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
            message: deploy_prefix,
            exit_code: 0,
            output_file: Some(reported_output_file),
            output_dir: reported_output_dir,
            launch_url: None,
            stdout: deploy_stdout,
            stderr: deploy_stderr,
        }),
    )
}

/// Outcome of a post-deploy monitor session.
#[derive(Debug)]
pub(crate) enum MonitorOutcome {
    /// halt-on-success pattern matched
    Success(String),
    /// halt-on-error pattern matched
    Error(String),
    /// Timeout reached
    Timeout { expect_found: bool },
}

pub(crate) struct MonitorState {
    halt_error_re: Option<regex::Regex>,
    halt_success_re: Option<regex::Regex>,
    expect_re: Option<regex::Regex>,
    start: std::time::Instant,
    timeout_dur: Option<std::time::Duration>,
    expect_found: bool,
    show_timestamp: bool,
}

impl MonitorState {
    pub(crate) fn new(
        timeout_secs: Option<f64>,
        halt_on_error: Option<&str>,
        halt_on_success: Option<&str>,
        expect: Option<&str>,
        show_timestamp: bool,
    ) -> Self {
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
        Self {
            halt_error_re,
            halt_success_re,
            expect_re,
            start: std::time::Instant::now(),
            timeout_dur: timeout_secs.map(std::time::Duration::from_secs_f64),
            expect_found: false,
            show_timestamp,
        }
    }

    pub(crate) fn timed_out(&self) -> bool {
        self.timeout_dur
            .is_some_and(|dur| self.start.elapsed() >= dur)
    }

    pub(crate) fn remaining(&self) -> Option<std::time::Duration> {
        self.timeout_dur
            .map(|dur| dur.saturating_sub(self.start.elapsed()))
    }

    pub(crate) fn timeout_outcome(&self) -> MonitorOutcome {
        MonitorOutcome::Timeout {
            expect_found: self.expect_found,
        }
    }

    pub(crate) fn expect_found(&self) -> bool {
        self.expect_found
    }

    pub(crate) fn process_line(&mut self, line: &str) -> Option<MonitorOutcome> {
        if self.show_timestamp {
            let total_secs = self.start.elapsed().as_secs_f64();
            let minutes = (total_secs / 60.0) as u64;
            let seconds = total_secs % 60.0;
            tracing::info!("{:02}:{:05.2} {}", minutes, seconds, line);
        } else {
            tracing::info!("{}", line);
        }

        if let Some(ref re) = self.expect_re {
            if re.is_match(line) {
                self.expect_found = true;
            }
        }

        if let Some(ref re) = self.halt_error_re {
            if re.is_match(line) {
                return Some(MonitorOutcome::Error(format!(
                    "halt-on-error pattern matched: {}",
                    line
                )));
            }
        }

        if let Some(ref re) = self.halt_success_re {
            if re.is_match(line) {
                return Some(MonitorOutcome::Success(format!(
                    "halt-on-success pattern matched: {}",
                    line
                )));
            }
        }

        None
    }
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
    let mut state = MonitorState::new(
        timeout_secs,
        halt_on_error,
        halt_on_success,
        expect,
        show_timestamp,
    );
    loop {
        if state.timed_out() {
            return state.timeout_outcome();
        }

        let recv_timeout = state
            .remaining()
            .unwrap_or(std::time::Duration::from_secs(1));

        match tokio::time::timeout(recv_timeout, rx.recv()).await {
            Ok(Ok(line)) => {
                if let Some(outcome) = state.process_line(&line) {
                    return outcome;
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                tracing::warn!("monitor lagged, skipped {} messages", n);
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return state.timeout_outcome();
            }
            Err(_) => {
                // Timeout on recv — check if overall timeout expired
                if state.timed_out() {
                    return state.timeout_outcome();
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

#[cfg(test)]
mod deploy_message_tests {
    //! Verifies the `/api/deploy` response message exposes the real
    //! deploy outcome (full / verify-skip / selective) instead of the
    //! generic `"deploy succeeded"`. See GitHub issue #76.
    //!
    //! These tests cover only the pure string-formatting contract; the
    //! underlying outcome computation is tested in `fbuild-deploy`.
    use fbuild_deploy::{esp32::FlashRegion, DeployOutcome};

    fn prefix_for(outcome: &DeployOutcome) -> String {
        format!("deploy succeeded ({})", outcome.describe())
    }

    #[test]
    fn full_flash_prefix() {
        assert_eq!(
            prefix_for(&DeployOutcome::FullFlash),
            "deploy succeeded (full flash)"
        );
    }

    #[test]
    fn verify_skip_prefix() {
        assert_eq!(
            prefix_for(&DeployOutcome::VerifySkip),
            "deploy succeeded (verify skipped, device already matched)"
        );
    }

    #[test]
    fn selective_flash_firmware_prefix() {
        let outcome = DeployOutcome::SelectiveFlash {
            regions: vec![FlashRegion::Firmware],
        };
        assert_eq!(
            prefix_for(&outcome),
            "deploy succeeded (selective flash: firmware)"
        );
    }

    #[test]
    fn monitor_suffix_preserved_on_selective_flash() {
        let outcome = DeployOutcome::SelectiveFlash {
            regions: vec![FlashRegion::Firmware],
        };
        let prefix = prefix_for(&outcome);
        let combined = format!("{}; monitor: ok", prefix);
        assert_eq!(
            combined,
            "deploy succeeded (selective flash: firmware); monitor: ok"
        );
    }

    #[test]
    fn monitor_error_suffix_preserved_on_verify_skip() {
        let prefix = prefix_for(&DeployOutcome::VerifySkip);
        let combined = format!("{}; monitor error: pattern matched", prefix);
        assert_eq!(
            combined,
            "deploy succeeded (verify skipped, device already matched); monitor error: pattern matched"
        );
    }
}
