//! `POST /api/deploy` (avr8js variant) — stages firmware for an in-browser
//! AVR8js session, optionally executes a headless run, and returns either a
//! launch URL or captured stdout/stderr.

use super::avr8js_headless::{run_avr8js_headless, RunAvr8jsHeadlessOptions, AVR8JS_HEADLESS_MJS};
use super::avr8js_npm::{ensure_avr8js_npm, find_node};
use super::avr8js_web::{now_unix, Avr8jsSessionManifest};
use crate::context::DaemonContext;
use crate::handlers::operations::MonitorOutcome;
use crate::models::OperationResponse;
use axum::http::StatusCode;
use axum::Json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub struct DeployAvr8jsRequest {
    pub request_id: String,
    pub project_dir: PathBuf,
    pub env_name: String,
    pub board_id: String,
    pub platform: fbuild_core::Platform,
    pub firmware_path: PathBuf,
    pub elf_path: Option<PathBuf>,
    pub monitor_after: bool,
    pub output_file: String,
    pub output_dir: Option<String>,
    pub monitor_timeout: Option<f64>,
    pub halt_on_error: Option<String>,
    pub halt_on_success: Option<String>,
    pub expect: Option<String>,
    pub show_timestamp: bool,
    pub verbose: bool,
}

pub async fn deploy_avr8js(
    ctx: Arc<DaemonContext>,
    req: DeployAvr8jsRequest,
) -> (StatusCode, Json<OperationResponse>) {
    let DeployAvr8jsRequest {
        request_id,
        project_dir,
        env_name,
        board_id,
        platform,
        firmware_path,
        elf_path,
        monitor_after,
        output_file,
        output_dir,
        monitor_timeout,
        halt_on_error,
        halt_on_success,
        expect,
        show_timestamp,
        verbose,
    } = req;

    if !matches!(
        platform,
        fbuild_core::Platform::AtmelAvr | fbuild_core::Platform::AtmelMegaAvr
    ) {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                "avr8js deploy target is only supported for AVR boards".to_string(),
            )),
        );
    }
    if board_id != "uno" {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!(
                    "avr8js deploy target currently supports only board 'uno' (got '{}')",
                    board_id
                ),
            )),
        );
    }
    if firmware_path.extension().and_then(|ext| ext.to_str()) != Some("hex") {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!(
                    "avr8js deploy target requires firmware.hex, got '{}'",
                    firmware_path.display()
                ),
            )),
        );
    }

    let board = match fbuild_config::BoardConfig::from_board_id_in_project(
        &board_id,
        &HashMap::new(),
        Some(project_dir.as_path()),
    ) {
        Ok(board) => board,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("failed to load AVR8js board config: {}", e),
                )),
            );
        }
    };
    if !board.mcu.eq_ignore_ascii_case("atmega328p") {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!(
                    "avr8js deploy target currently supports only ATmega328P, got '{}'",
                    board.mcu
                ),
            )),
        );
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    let session_dir = fbuild_paths::get_project_fbuild_dir(&project_dir)
        .join("emulators")
        .join("avr8js")
        .join(&env_name)
        .join(&session_id);
    if let Err(e) = std::fs::create_dir_all(&session_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("failed to create AVR8js session dir: {}", e),
            )),
        );
    }

    let staged_hex = session_dir.join("firmware.hex");
    if let Err(e) = std::fs::copy(&firmware_path, &staged_hex) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("failed to stage AVR8js firmware.hex: {}", e),
            )),
        );
    }

    let staged_elf = if let Some(ref elf) = elf_path {
        let dest = session_dir.join("firmware.elf");
        match std::fs::copy(elf, &dest) {
            Ok(_) => Some(dest),
            Err(_) => None,
        }
    } else {
        None
    };

    let manifest = Avr8jsSessionManifest {
        session_id: session_id.clone(),
        project_dir: project_dir.to_string_lossy().to_string(),
        env_name: env_name.clone(),
        board_id,
        platform: format!("{:?}", platform),
        mcu: board.mcu.clone(),
        f_cpu_hz: board
            .f_cpu
            .trim_end_matches('L')
            .parse::<u32>()
            .unwrap_or(16_000_000),
        firmware_hex: staged_hex.to_string_lossy().to_string(),
        firmware_elf: staged_elf.map(|p| p.to_string_lossy().to_string()),
        created_at_unix: now_unix(),
    };
    let manifest_path = session_dir.join("session.json");
    if let Err(e) = std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    ) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("failed to write AVR8js session manifest: {}", e),
            )),
        );
    }
    ctx.avr8js_sessions
        .insert(session_id.clone(), manifest_path);

    if monitor_after {
        // Headless path: run avr8js in Node.js subprocess, capture UART on stdout
        let node_path = match find_node() {
            Ok(p) => p,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(OperationResponse::fail(request_id, e.to_string())),
                );
            }
        };
        let avr8js_cache = match ensure_avr8js_npm() {
            Ok(p) => p,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(request_id, e.to_string())),
                );
            }
        };

        // headless.mjs uses `import ... from "avr8js"` (bare ESM specifier).
        // Node's ESM resolver does NOT honor NODE_PATH — it only walks
        // upward from the script's directory looking for node_modules.
        // Stage the script inside the cache dir so node_modules/avr8js
        // is on that walk-up path. Script content is a compile-time
        // constant (`include_str!`), so concurrent writes are idempotent.
        // The per-session firmware.hex and session.json continue to live
        // under session_dir. See FastLED/fbuild#291.
        let script_path = avr8js_cache.join("headless.mjs");
        if let Err(e) = std::fs::write(&script_path, AVR8JS_HEADLESS_MJS) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("failed to write headless.mjs: {}", e),
                )),
            );
        }

        let avr8js_result = match run_avr8js_headless(
            &node_path,
            &script_path,
            &staged_hex,
            manifest.f_cpu_hz,
            &avr8js_cache,
            RunAvr8jsHeadlessOptions {
                timeout_secs: monitor_timeout,
                halt_on_error: halt_on_error.as_deref(),
                halt_on_success: halt_on_success.as_deref(),
                expect: expect.as_deref(),
                show_timestamp,
                verbose,
            },
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(request_id, e.to_string())),
                );
            }
        };

        match avr8js_result.outcome {
            MonitorOutcome::Success(message) => (
                StatusCode::OK,
                Json(OperationResponse {
                    success: true,
                    request_id,
                    message: format!("avr8js run succeeded: {}", message),
                    exit_code: 0,
                    output_file: Some(output_file),
                    output_dir,
                    launch_url: None,
                    stdout: Some(avr8js_result.stdout),
                    stderr: Some(avr8js_result.stderr),
                }),
            ),
            MonitorOutcome::Error(message) => (
                StatusCode::OK,
                Json(OperationResponse {
                    success: false,
                    request_id,
                    message: format!("avr8js run failed: {}", message),
                    exit_code: 1,
                    output_file: Some(output_file),
                    output_dir,
                    launch_url: None,
                    stdout: Some(avr8js_result.stdout),
                    stderr: Some(avr8js_result.stderr),
                }),
            ),
            MonitorOutcome::Timeout { expect_found } => {
                let success = expect.is_none() || expect_found;
                let exit_code = if success { 0 } else { 1 };
                (
                    StatusCode::OK,
                    Json(OperationResponse {
                        success,
                        request_id,
                        message: if success {
                            "avr8js run completed (timeout)".to_string()
                        } else {
                            "avr8js run timed out (expected pattern not found)".to_string()
                        },
                        exit_code,
                        output_file: Some(output_file),
                        output_dir,
                        launch_url: None,
                        stdout: Some(avr8js_result.stdout),
                        stderr: Some(avr8js_result.stderr),
                    }),
                )
            }
            // avr8js has no real ESP DTR/RTS lines; the auto-recover signal
            // is unreachable from this emulator path. Defensive arm.
            MonitorOutcome::RecoverDownloadMode { signal } => (
                StatusCode::OK,
                Json(OperationResponse {
                    success: false,
                    request_id,
                    message: format!(
                        "internal: avr8js emitted ESP RecoverDownloadMode ({})",
                        signal.diagnostic()
                    ),
                    exit_code: 1,
                    output_file: Some(output_file),
                    output_dir,
                    launch_url: None,
                    stdout: Some(avr8js_result.stdout),
                    stderr: Some(avr8js_result.stderr),
                }),
            ),
        }
    } else {
        // Browser path: return URL for the avr8js web UI
        let launch_url = Some(format!(
            "http://127.0.0.1:{}/emulator/avr8js/{}",
            ctx.port, session_id
        ));
        (
            StatusCode::OK,
            Json(OperationResponse {
                success: true,
                request_id,
                message: "deploy complete".to_string(),
                exit_code: 0,
                output_file: Some(output_file),
                output_dir,
                launch_url,
                stdout: None,
                stderr: None,
            }),
        )
    }
}
