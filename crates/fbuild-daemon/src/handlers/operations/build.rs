//! `POST /api/build` — kick off a build (streaming or buffered).

use super::common::{export_artifacts_bundle, resolve_client_path, OperationGuard};
use crate::context::DaemonContext;
use crate::models::{BuildRequest, OperationResponse};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::PathBuf;
use std::sync::Arc;

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
            watch_set_cache: Some(Arc::clone(&ctx.watch_set_cache) as Arc<_>),
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
            // Daemon state goes to `Building` *before* the lock is taken
            // (the OperationGuard above). Without per-phase tracing, an
            // indefinite stall here looks identical to an indefinite
            // stall inside the build itself — both surface as
            // "State: building" forever. Emit a tracing event around the
            // lock acquire so daemon.log shows when the daemon is queued
            // behind another build vs. actually compiling. See #346
            // finding (2). The 10s warn-threshold is well above normal
            // contention but below any human-perceptible "is it hung?"
            // window, so a stuck lock surfaces in the log within seconds
            // instead of being invisible.
            let lock_wait_start = std::time::Instant::now();
            let lock = ctx.project_lock(&project_dir);
            tracing::info!("waiting for project lock on {}", project_dir_desc);
            const LOCK_WAIT_WARN: std::time::Duration = std::time::Duration::from_secs(10);
            let _lock_guard = {
                let acquire = lock.lock();
                match tokio::time::timeout(LOCK_WAIT_WARN, acquire).await {
                    Ok(guard) => guard,
                    Err(_) => {
                        tracing::warn!(
                            "project lock on {} not acquired within {}s — \
                             another build is still holding it. Continuing \
                             to wait; if this persists the daemon may be \
                             stuck. Inspect ~/.fbuild/<env>/daemon/daemon.log \
                             or run `fbuild daemon locks` to see who is \
                             holding the lock.",
                            project_dir_desc,
                            LOCK_WAIT_WARN.as_secs(),
                        );
                        lock.lock().await
                    }
                }
            };
            let lock_wait = lock_wait_start.elapsed();
            tracing::info!(
                "project lock acquired on {} after {} ms",
                project_dir_desc,
                lock_wait.as_millis()
            );

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

            if !success && !msg.is_empty() {
                let log_event = serde_json::json!({
                    "type": "log",
                    "message": msg,
                });
                let mut chunk = log_event.to_string();
                chunk.push('\n');
                let _ = async_tx.send(bytes::Bytes::from(chunk));
            }

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
            watch_set_cache: Some(Arc::clone(&ctx.watch_set_cache) as Arc<_>),
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
