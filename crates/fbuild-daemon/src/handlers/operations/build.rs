//! `POST /api/build` — kick off a build (streaming or buffered).

use super::common::{
    export_artifacts_bundle, resolve_build_dir, resolve_client_path, OperationGuard,
};
use crate::context::DaemonContext;
use crate::models::{BuildRequest, OperationResponse};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Ensures a terminal `result` NDJSON event reaches the client, even if the
/// streaming task panics or returns early. Without this, the body stream
/// closes mid-frame and the client sees the opaque
/// `stream error: error decoding response body` from reqwest with no clue
/// what went wrong. See fbuild#401.
struct StreamTerminationGuard {
    tx: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
    request_id: String,
    completed: bool,
}

impl StreamTerminationGuard {
    fn new(tx: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>, request_id: String) -> Self {
        Self {
            tx,
            request_id,
            completed: false,
        }
    }

    fn mark_completed(&mut self) {
        self.completed = true;
    }
}

impl Drop for StreamTerminationGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let event = serde_json::json!({
            "type": "result",
            "success": false,
            "request_id": self.request_id,
            "message": "daemon build worker terminated unexpectedly (panic or early return); check ~/.fbuild/daemon/daemon.log",
            "exit_code": 1,
            "output_file": null,
            "output_dir": null,
        });
        let mut chunk = event.to_string();
        chunk.push('\n');
        let _ = self.tx.send(bytes::Bytes::from(chunk));
    }
}

fn send_stream_status_event(
    tx: &tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
    ctx: &DaemonContext,
    request_id: &str,
    message: impl Into<String>,
) {
    let current_operation = ctx.current_operation.read().ok().and_then(|op| op.clone());
    let operation_in_progress = ctx.operation_in_progress.load(Ordering::Relaxed);
    let dependency_install = ctx.dependency_install_snapshot();
    let event = serde_json::json!({
        "type": "status",
        "request_id": request_id,
        "message": message.into(),
        "current_operation": current_operation,
        "operation_in_progress": operation_in_progress,
        "dependency_install": dependency_install,
    });
    let mut chunk = event.to_string();
    chunk.push('\n');
    let _ = tx.send(bytes::Bytes::from(chunk));
}

fn dependency_install_message(status: &fbuild_core::install_status::InstallStatus) -> String {
    match status.version.as_deref() {
        Some(version) => format!("{} {}: {}", status.name, version, status.message),
        None => format!("{}: {}", status.name, status.message),
    }
}

fn should_emit_dependency_status(status: &fbuild_core::install_status::InstallStatus) -> bool {
    !matches!(
        status.phase,
        fbuild_core::install_status::InstallPhase::Installed
            | fbuild_core::install_status::InstallPhase::Failed
    )
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

    let build_dir = resolve_build_dir(
        req.build_dir_override.as_deref(),
        req.flatten_env,
        req.caller_cwd.as_deref(),
        &project_dir,
        &env_name,
        profile,
    );
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
        //
        // fbuild#818 async-audit follow-up: `BuildLog`'s sender is now a
        // `tokio::sync::mpsc::UnboundedSender`, so the orchestrator (which
        // may push from blocking compile workers via `spawn_blocking`) and
        // this async forwarder share a single tokio channel. The previous
        // `std::sync::mpsc::channel` + `spawn_blocking` recv-bridge has
        // been removed — `UnboundedSender::send` is sync and callable from
        // any context, and `UnboundedReceiver::recv` is awaited directly
        // from the async forwarder task.
        let (log_tx, mut log_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
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
            log_sender: Some(log_tx),
            symbol_analysis: req.symbol_analysis,
            symbol_analysis_path: resolved_symbol_analysis_path.clone(),
            no_timestamp: req.no_timestamp,
            src_dir: req.src_dir.clone(),
            pio_env: req.pio_env.clone(),
            extra_build_flags: Vec::new(),
            watch_set_cache: Some(Arc::clone(&ctx.watch_set_cache) as Arc<_>),
            bloat_analysis: req.bloat_analysis,
        };

        let project_dir_desc = req.project_dir.clone();
        let guard_request_id = request_id.clone();
        tokio::spawn(async move {
            let mut termination_guard =
                StreamTerminationGuard::new(async_tx.clone(), guard_request_id);
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
            const STREAM_STATUS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
            // FastLED/fbuild#808 (CRITICAL): hard ceiling on lock-wait
            // so a wedged previous build (esptool/avrdude hung at the
            // OS-driver level) can't keep every subsequent build for
            // the same project queued forever. 30 min is well above
            // any legitimate cold build and well below "daemon is
            // structurally stuck".
            const LOCK_WAIT_HARD_DEADLINE: std::time::Duration =
                std::time::Duration::from_secs(30 * 60);
            let lock_guard_result = {
                let mut acquire = Box::pin(lock.lock());
                let mut warned = false;
                let mut acquired: Option<_> = None;
                loop {
                    if lock_wait_start.elapsed() >= LOCK_WAIT_HARD_DEADLINE {
                        break;
                    }
                    match tokio::time::timeout(LOCK_WAIT_WARN, &mut acquire).await {
                        Ok(guard) => {
                            acquired = Some(guard);
                            break;
                        }
                        Err(_) => {
                            let elapsed = lock_wait_start.elapsed();
                            if !warned {
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
                                warned = true;
                            }
                            send_stream_status_event(
                                &async_tx,
                                &ctx,
                                &request_id,
                                format!(
                                    "waiting for another build of {} to finish ({}s)",
                                    project_dir_desc,
                                    elapsed.as_secs()
                                ),
                            );
                        }
                    }
                }
                acquired
            };
            let _lock_guard = match lock_guard_result {
                Some(g) => g,
                None => {
                    let msg = format!(
                        "project lock for {} not acquired within {}s; previous build may be wedged — \
                         run `fbuild daemon locks` to see who is holding it",
                        project_dir_desc,
                        LOCK_WAIT_HARD_DEADLINE.as_secs()
                    );
                    tracing::error!("{}", msg);
                    let result_event = serde_json::json!({
                        "type": "result",
                        "success": false,
                        "request_id": request_id.clone(),
                        "message": msg,
                        "exit_code": 1,
                        "output_file": null,
                        "output_dir": null,
                    });
                    let mut chunk = result_event.to_string();
                    chunk.push('\n');
                    let _ = async_tx.send(bytes::Bytes::from(chunk));
                    termination_guard.mark_completed();
                    return;
                }
            };
            let lock_wait = lock_wait_start.elapsed();
            if lock_wait >= STREAM_STATUS_INTERVAL {
                send_stream_status_event(
                    &async_tx,
                    &ctx,
                    &request_id,
                    format!(
                        "project lock acquired for {} after {}s",
                        project_dir_desc,
                        lock_wait.as_secs()
                    ),
                );
            }
            tracing::info!(
                "project lock acquired on {} after {} ms",
                project_dir_desc,
                lock_wait.as_millis()
            );

            // Forwarder: log lines → async NDJSON chunks.
            // fbuild#818: both endpoints are now tokio channels, so the
            // earlier `spawn_blocking` sync→async bridge is gone — this
            // task just awaits on the same runtime.
            let forwarder_tx = async_tx.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(line) = log_rx.recv().await {
                    let event = serde_json::json!({"type": "log", "message": line});
                    let mut chunk = event.to_string();
                    chunk.push('\n');
                    if forwarder_tx.send(bytes::Bytes::from(chunk)).is_err() {
                        break;
                    }
                }
            });

            // Run build. fbuild#813 / #815: the orchestrator is now async,
            // so we spawn it directly on the runtime. The status-heartbeat
            // loop still needs an awaitable handle, so wrap the build
            // future in `tokio::spawn` and poll it via timeout-driven
            // selection.
            let build_wallclock_start = std::time::Instant::now();
            let mut build_task = tokio::spawn(async move {
                let orchestrator = fbuild_build::get_orchestrator(platform)?;
                orchestrator.build(&params).await
            });
            // FastLED/fbuild#808 (CRITICAL): wall-clock cap on the
            // streaming build so a wedged C compiler (process stuck
            // on an OS lock) cannot keep the handler awake forever.
            // 60 min is well above any legitimate cold-toolchain
            // first build and well below "daemon is hung".
            const BUILD_HARD_DEADLINE: std::time::Duration =
                std::time::Duration::from_secs(60 * 60);
            let build_result = loop {
                if build_wallclock_start.elapsed() >= BUILD_HARD_DEADLINE {
                    build_task.abort();
                    let abort_msg = format!(
                        "build exceeded hard deadline ({}s); aborting — a compiler may be wedged",
                        BUILD_HARD_DEADLINE.as_secs()
                    );
                    tracing::error!("{}", abort_msg);
                    break Ok(Err(fbuild_core::FbuildError::Other(abort_msg)));
                }
                match tokio::time::timeout(STREAM_STATUS_INTERVAL, &mut build_task).await {
                    Ok(result) => break result,
                    Err(_) => {
                        if let Some(status) = ctx.dependency_install_snapshot() {
                            if should_emit_dependency_status(&status) {
                                send_stream_status_event(
                                    &async_tx,
                                    &ctx,
                                    &request_id,
                                    dependency_install_message(&status),
                                );
                            }
                        }
                    }
                }
            };
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
                            // fbuild#815: export_artifacts_bundle does sync
                            // std::fs I/O — move it off the axum worker.
                            let out_dir_owned = out_dir.clone();
                            let env_name_owned = env_name.clone();
                            let firmware_path_owned = br.firmware_path.clone();
                            let elf_path_owned = br.elf_path.clone();
                            let join_result = tokio::task::spawn_blocking(move || {
                                export_artifacts_bundle(
                                    &out_dir_owned,
                                    platform,
                                    &env_name_owned,
                                    firmware_path_owned.as_deref(),
                                    elf_path_owned.as_deref(),
                                )
                            })
                            .await;
                            Some(match join_result {
                                Ok(inner) => inner,
                                Err(join_err) => Err(fbuild_core::FbuildError::Other(format!(
                                    "artifact export task panicked: {}",
                                    join_err
                                ))),
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let _lines = br.build_log.into_lines(); // drop sender so forwarder exits
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

            let _ = forwarder.await;

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

            // Final event sent successfully — disarm the termination guard so
            // its Drop impl does not emit a duplicate fallback event.
            termination_guard.mark_completed();
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
        // FastLED/fbuild#808 (CRITICAL): hard ceiling on the project
        // lock so a wedged previous build cannot wedge every
        // subsequent build of the same project. Mirrors the streaming
        // path's `LOCK_WAIT_HARD_DEADLINE`.
        const NON_STREAM_LOCK_HARD_DEADLINE: std::time::Duration =
            std::time::Duration::from_secs(30 * 60);
        let lock = ctx.project_lock(&project_dir);
        let _guard = match tokio::time::timeout(NON_STREAM_LOCK_HARD_DEADLINE, lock.lock()).await {
            Ok(g) => g,
            Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(OperationResponse::fail(
                        request_id,
                        format!(
                            "project lock not acquired within {}s; previous build may be wedged — \
                             run `fbuild daemon locks` to see who is holding it",
                            NON_STREAM_LOCK_HARD_DEADLINE.as_secs()
                        ),
                    )),
                )
                    .into_response();
            }
        };

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
            bloat_analysis: req.bloat_analysis,
        };

        // fbuild#813 / #815: orchestrator.build is async, call directly.
        // FastLED/fbuild#808 (CRITICAL): wall-clock cap on the build so
        // a wedged compiler cannot lock up the HTTP handler indefinitely.
        const NON_STREAM_BUILD_HARD_DEADLINE: std::time::Duration =
            std::time::Duration::from_secs(60 * 60);
        let result = match fbuild_build::get_orchestrator(platform) {
            Ok(orch) => match tokio::time::timeout(
                NON_STREAM_BUILD_HARD_DEADLINE,
                orch.build(&params),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(fbuild_core::FbuildError::Other(format!(
                    "build exceeded hard deadline ({}s); aborting — a compiler may be wedged",
                    NON_STREAM_BUILD_HARD_DEADLINE.as_secs()
                ))),
            },
            Err(e) => Err(e),
        };

        match result {
            Ok(build_result) => {
                let exported = if build_result.success {
                    if let Some(ref out_dir) = resolved_output_dir {
                        // fbuild#815: export_artifacts_bundle does sync
                        // std::fs I/O — move it off the axum worker.
                        let out_dir_owned = out_dir.clone();
                        let env_name_owned = env_name.clone();
                        let firmware_path_owned = build_result.firmware_path.clone();
                        let elf_path_owned = build_result.elf_path.clone();
                        let join_result = tokio::task::spawn_blocking(move || {
                            export_artifacts_bundle(
                                &out_dir_owned,
                                platform,
                                &env_name_owned,
                                firmware_path_owned.as_deref(),
                                elf_path_owned.as_deref(),
                            )
                        })
                        .await;
                        Some(match join_result {
                            Ok(inner) => inner,
                            Err(join_err) => Err(fbuild_core::FbuildError::Other(format!(
                                "artifact export task panicked: {}",
                                join_err
                            ))),
                        })
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
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("build error: {}", e),
                )),
            )
                .into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fbuild#401: when the streaming task panics or returns early without
    /// emitting the result event, the guard's Drop impl must enqueue a
    /// fallback terminal event so the CLI sees a meaningful error.
    #[test]
    fn termination_guard_emits_fallback_on_drop_when_not_completed() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        {
            let _guard = StreamTerminationGuard::new(tx, "req-123".to_string());
            // Simulate panic / early return: drop without calling mark_completed.
        }
        let chunk = rx.try_recv().expect("guard should enqueue fallback event");
        let line = std::str::from_utf8(&chunk).unwrap().trim_end();
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(event["type"], "result");
        assert_eq!(event["success"], false);
        assert_eq!(event["exit_code"], 1);
        assert_eq!(event["request_id"], "req-123");
        let msg = event["message"].as_str().unwrap();
        assert!(
            msg.contains("terminated unexpectedly"),
            "fallback message should be actionable, got: {msg}"
        );
    }

    /// After mark_completed, drop must NOT emit a duplicate event.
    #[test]
    fn termination_guard_silent_on_drop_when_completed() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        {
            let mut guard = StreamTerminationGuard::new(tx, "req-456".to_string());
            guard.mark_completed();
        }
        assert!(
            rx.try_recv().is_err(),
            "completed guard must not enqueue a fallback event"
        );
    }

    #[test]
    fn stream_status_event_includes_dependency_install_snapshot() {
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        let ctx = DaemonContext::new(0, shutdown_tx, ".".to_string());
        ctx.set_dependency_install(fbuild_core::install_status::status(
            "zccache",
            Some("0.9.1"),
            fbuild_core::install_status::InstallPhase::WaitingForLock,
            fbuild_core::install_status::InstallRole::Waiter,
            "waiting for zccache download lock",
            Some(".zccache.install.lock"),
        ));

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        send_stream_status_event(&tx, &ctx, "req-789", "waiting for dependency install");

        let chunk = rx.try_recv().expect("status event should be queued");
        let line = std::str::from_utf8(&chunk).unwrap().trim_end();
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(event["type"], "status");
        assert_eq!(event["request_id"], "req-789");
        assert_eq!(event["message"], "waiting for dependency install");
        assert_eq!(event["dependency_install"]["name"], "zccache");
        assert_eq!(event["dependency_install"]["phase"], "waiting_for_lock");
    }

    #[test]
    fn dependency_status_heartbeat_skips_terminal_phases() {
        let waiting = fbuild_core::install_status::status(
            "toolchain",
            Some("1.0"),
            fbuild_core::install_status::InstallPhase::WaitingForLock,
            fbuild_core::install_status::InstallRole::Waiter,
            "waiting",
            None::<String>,
        );
        let installed = fbuild_core::install_status::status(
            "toolchain",
            Some("1.0"),
            fbuild_core::install_status::InstallPhase::Installed,
            fbuild_core::install_status::InstallRole::Installer,
            "installed",
            None::<String>,
        );
        assert!(should_emit_dependency_status(&waiting));
        assert!(!should_emit_dependency_status(&installed));
    }
}
