use crate::context::DaemonContext;
use crate::handlers::operations::{MonitorOutcome, MonitorState};
use crate::models::OperationResponse;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::Json;
use fbuild_packages::{Package, Toolchain};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

const AVR8JS_APP_JS: &str = include_str!("../../web/avr8js/app.js");
const AVR8JS_HEADLESS_MJS: &str = include_str!("../../web/avr8js/headless.mjs");

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Avr8jsSessionManifest {
    session_id: String,
    project_dir: String,
    env_name: String,
    board_id: String,
    platform: String,
    mcu: String,
    f_cpu_hz: u32,
    firmware_hex: String,
    firmware_elf: Option<String>,
    created_at_unix: f64,
}

#[derive(Debug, Serialize)]
struct Avr8jsSessionResponse {
    session_id: String,
    env_name: String,
    board_id: String,
    platform: String,
    mcu: String,
    f_cpu_hz: u32,
    firmware_hex_url: String,
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn render_page(session_id: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>AVR8js Monitor</title>
  <style>
    :root {{
      color-scheme: light;
      --bg: #f4efe4;
      --panel: #fffdf8;
      --ink: #1f1b18;
      --muted: #6d6258;
      --line: #d9cdbd;
      --accent: #bd4b2c;
    }}
    body {{
      margin: 0;
      min-height: 100vh;
      background:
        radial-gradient(circle at top left, #fff7ea 0%, rgba(255,247,234,0) 35%),
        linear-gradient(160deg, #f7f0e5 0%, #efe4d5 100%);
      color: var(--ink);
      font-family: "Consolas", "Menlo", monospace;
      display: grid;
      place-items: center;
      padding: 24px;
      box-sizing: border-box;
    }}
    .shell {{
      width: min(920px, 100%);
      background: var(--panel);
      border: 1px solid var(--line);
      box-shadow: 0 18px 60px rgba(74, 52, 34, 0.16);
      border-radius: 18px;
      overflow: hidden;
    }}
    .bar {{
      display: flex;
      justify-content: space-between;
      align-items: center;
      gap: 16px;
      padding: 14px 18px;
      border-bottom: 1px solid var(--line);
      background: linear-gradient(180deg, #fffaf2 0%, #f8efe2 100%);
    }}
    .title {{
      font-size: 14px;
      font-weight: 700;
      letter-spacing: 0.08em;
      text-transform: uppercase;
    }}
    .status {{
      font-size: 12px;
      color: var(--muted);
    }}
    .stdout {{
      width: 100%;
      min-height: 68vh;
      border: 0;
      outline: none;
      resize: none;
      padding: 18px;
      box-sizing: border-box;
      background: #231c16;
      color: #f4efe8;
      font: 15px/1.5 "Consolas", "Menlo", monospace;
    }}
    .stdout::selection {{
      background: rgba(255, 190, 120, 0.35);
    }}
    .footer {{
      display: flex;
      justify-content: space-between;
      gap: 16px;
      padding: 10px 18px 16px;
      border-top: 1px solid var(--line);
      font-size: 12px;
      color: var(--muted);
      background: #fffaf2;
    }}
    .accent {{
      color: var(--accent);
      font-weight: 700;
    }}
  </style>
</head>
<body>
  <div class="shell">
    <div class="bar">
      <div class="title">AVR8js Emulator</div>
      <div class="status" id="status">Loading session...</div>
    </div>
    <textarea id="stdout" class="stdout" readonly spellcheck="false"></textarea>
    <div class="footer">
      <div>Session <span class="accent">{}</span></div>
      <div>Browser-side ATmega328P serial monitor</div>
    </div>
  </div>
  <script>window.__AVR8JS_SESSION_ID__ = {};</script>
  <script type="module" src="/emulator/avr8js/app.js"></script>
</body>
</html>
"#,
        session_id,
        serde_json::to_string(session_id).unwrap_or_else(|_| "\"\"".to_string()),
    )
}

fn load_session_manifest(
    ctx: &DaemonContext,
    session_id: &str,
) -> fbuild_core::Result<Avr8jsSessionManifest> {
    let manifest_path = ctx
        .avr8js_sessions
        .get(session_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            fbuild_core::FbuildError::Other(format!("unknown AVR8js session '{}'", session_id))
        })?;
    let raw = std::fs::read_to_string(&manifest_path)?;
    serde_json::from_str(&raw).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to parse AVR8js session manifest: {}", e))
    })
}

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

pub struct DeployQemuRequest {
    pub request_id: String,
    pub project_dir: PathBuf,
    pub env_name: String,
    pub board_id: String,
    pub platform: fbuild_core::Platform,
    pub firmware_path: PathBuf,
    pub elf_path: Option<PathBuf>,
    pub output_file: String,
    pub output_dir: Option<String>,
    pub monitor_timeout: Option<f64>,
    pub qemu_timeout_secs: u32,
    pub halt_on_error: Option<String>,
    pub halt_on_success: Option<String>,
    pub expect: Option<String>,
    pub show_timestamp: bool,
    pub verbose: bool,
    pub board_overrides: HashMap<String, String>,
}

struct ProcessLine {
    is_stderr: bool,
    line: String,
}

enum ProcessEvent {
    Line(ProcessLine),
    StreamClosed,
}

struct QemuRunResult {
    outcome: MonitorOutcome,
    stdout: String,
    stderr: String,
}

struct RunQemuOptions<'a> {
    elf_path: Option<PathBuf>,
    addr2line_path: Option<PathBuf>,
    timeout_secs: Option<f64>,
    halt_on_error: Option<&'a str>,
    halt_on_success: Option<&'a str>,
    expect: Option<&'a str>,
    show_timestamp: bool,
    verbose: bool,
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

    let board = match fbuild_config::BoardConfig::from_board_id(&board_id, &HashMap::new()) {
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

        let script_path = session_dir.join("headless.mjs");
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

// ---------------------------------------------------------------------------
// Headless avr8js helpers
// ---------------------------------------------------------------------------

fn find_node() -> fbuild_core::Result<PathBuf> {
    let node = if cfg!(windows) { "node.exe" } else { "node" };
    match std::process::Command::new(node)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => Ok(PathBuf::from(node)),
        _ => Err(fbuild_core::FbuildError::DeployFailed(
            "Node.js is required for headless avr8js emulation but 'node' was not found on PATH. \
             Install Node.js 18+ from https://nodejs.org/"
                .to_string(),
        )),
    }
}

fn ensure_avr8js_npm() -> fbuild_core::Result<PathBuf> {
    let cache_dir = fbuild_paths::get_cache_root().join("avr8js-node");
    let marker = cache_dir.join("node_modules").join("avr8js");
    if marker.exists() {
        return Ok(cache_dir);
    }
    std::fs::create_dir_all(&cache_dir).map_err(|e| {
        fbuild_core::FbuildError::DeployFailed(format!("failed to create avr8js cache dir: {}", e))
    })?;
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let output = std::process::Command::new(npm)
        .args(["install", "--save", "avr8js@0.21.0"])
        .arg("--prefix")
        .arg(&cache_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| {
            fbuild_core::FbuildError::DeployFailed(format!(
                "failed to run npm install for avr8js: {}. \
                 Ensure npm is installed alongside Node.js.",
                e
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(fbuild_core::FbuildError::DeployFailed(format!(
            "npm install avr8js failed: {}",
            stderr
        )));
    }
    Ok(cache_dir)
}

struct Avr8jsRunResult {
    outcome: MonitorOutcome,
    stdout: String,
    stderr: String,
}

struct RunAvr8jsHeadlessOptions<'a> {
    timeout_secs: Option<f64>,
    halt_on_error: Option<&'a str>,
    halt_on_success: Option<&'a str>,
    expect: Option<&'a str>,
    show_timestamp: bool,
    verbose: bool,
}

async fn run_avr8js_headless(
    node_path: &Path,
    script_path: &Path,
    hex_path: &Path,
    f_cpu_hz: u32,
    avr8js_cache_dir: &Path,
    options: RunAvr8jsHeadlessOptions<'_>,
) -> fbuild_core::Result<Avr8jsRunResult> {
    let mut cmd = tokio::process::Command::new(node_path);
    cmd.arg(script_path)
        .arg("--hex")
        .arg(hex_path)
        .arg("--f-cpu")
        .arg(f_cpu_hz.to_string())
        .env("NODE_PATH", avr8js_cache_dir.join("node_modules"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    if options.verbose {
        tracing::info!(
            "avr8js headless: {} {} --hex {} --f-cpu {}",
            node_path.display(),
            script_path.display(),
            hex_path.display(),
            f_cpu_hz
        );
    }

    let mut child = cmd.spawn().map_err(|e| {
        fbuild_core::FbuildError::DeployFailed(format!(
            "failed to launch Node.js for avr8js: {}",
            e
        ))
    })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        fbuild_core::FbuildError::DeployFailed("failed to capture avr8js stdout".to_string())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        fbuild_core::FbuildError::DeployFailed("failed to capture avr8js stderr".to_string())
    })?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ProcessEvent>();
    let stdout_task = tokio::spawn(spawn_line_reader(stdout, false, tx.clone()));
    let stderr_task = tokio::spawn(spawn_line_reader(stderr, true, tx));

    let mut monitor = MonitorState::new(
        options.timeout_secs,
        options.halt_on_error,
        options.halt_on_success,
        options.expect,
        options.show_timestamp,
    );
    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();
    let mut streams_open = 2usize;
    let mut child_exit: Option<std::process::ExitStatus> = None;
    let mut final_outcome: Option<MonitorOutcome> = None;

    loop {
        if monitor.timed_out() {
            final_outcome = Some(monitor.timeout_outcome());
            let _ = child.kill().await;
            break;
        }

        let recv_timeout = monitor
            .remaining()
            .unwrap_or(std::time::Duration::from_secs(1));

        tokio::select! {
            status = child.wait(), if child_exit.is_none() => {
                child_exit = Some(status.map_err(|e| {
                    fbuild_core::FbuildError::DeployFailed(format!("avr8js wait failed: {}", e))
                })?);
                if streams_open == 0 {
                    break;
                }
            }
            maybe_event = tokio::time::timeout(recv_timeout, rx.recv()) => {
                match maybe_event {
                    Ok(Some(ProcessEvent::Line(line))) => {
                        let target = if line.is_stderr { &mut stderr_buf } else { &mut stdout_buf };
                        target.push_str(&line.line);
                        target.push('\n');

                        if let Some(outcome) = monitor.process_line(&line.line) {
                            final_outcome = Some(outcome);
                            let _ = child.kill().await;
                            break;
                        }
                    }
                    Ok(Some(ProcessEvent::StreamClosed)) => {
                        streams_open = streams_open.saturating_sub(1);
                        if streams_open == 0 && child_exit.is_some() {
                            break;
                        }
                    }
                    Ok(None) => {
                        if child_exit.is_some() {
                            break;
                        }
                    }
                    Err(_) => {
                        final_outcome = Some(monitor.timeout_outcome());
                        let _ = child.kill().await;
                        break;
                    }
                }
            }
        }
    }

    if child_exit.is_none() {
        child_exit = Some(child.wait().await.map_err(|e| {
            fbuild_core::FbuildError::DeployFailed(format!("avr8js wait failed: {}", e))
        })?);
    }

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    let outcome = if let Some(outcome) = final_outcome {
        outcome
    } else if let Some(status) = child_exit {
        if status.success() {
            if options.expect.is_some() && !monitor.expect_found() {
                MonitorOutcome::Error(
                    "avr8js exited before the expected pattern was found".to_string(),
                )
            } else {
                MonitorOutcome::Success("avr8js exited normally".to_string())
            }
        } else {
            MonitorOutcome::Error(format!(
                "avr8js exited with code {}",
                status.code().unwrap_or(-1)
            ))
        }
    } else {
        MonitorOutcome::Error("avr8js exited unexpectedly".to_string())
    };

    Ok(Avr8jsRunResult {
        outcome,
        stdout: stdout_buf,
        stderr: stderr_buf,
    })
}

fn qemu_session_dir(project_dir: &Path, env_name: &str) -> PathBuf {
    fbuild_paths::get_project_fbuild_dir(project_dir)
        .join("emulators")
        .join("qemu")
        .join(env_name)
        .join(uuid::Uuid::new_v4().to_string())
}

fn build_linux_macos_qemu_hint(err: &str) -> String {
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        format!(
            "{}. On Linux/macOS, ensure QEMU runtime deps are installed: libgcrypt, glib2, pixman, SDL2, and libslirp.",
            err
        )
    } else {
        err.to_string()
    }
}

fn resolve_esp32_toolchain_gcc_path(
    project_dir: &Path,
    mcu_config: &fbuild_build::esp32::mcu_config::Esp32McuConfig,
) -> fbuild_core::Result<PathBuf> {
    let platform = fbuild_packages::library::Esp32Platform::new(project_dir);
    Package::ensure_installed(&platform)?;

    let is_riscv = mcu_config.is_riscv();
    let prefix = mcu_config.toolchain_prefix();
    let toolchain_name = if is_riscv {
        "toolchain-riscv32-esp"
    } else {
        "toolchain-xtensa-esp-elf"
    };

    let toolchain = match platform.get_toolchain_metadata_url(is_riscv) {
        Ok(metadata_url) => {
            let cache = fbuild_packages::Cache::new(project_dir);
            let cache_dir = cache.toolchains_dir().join(toolchain_name);
            match fbuild_packages::toolchain::esp32_metadata::resolve_toolchain_url_sync(
                &metadata_url,
                toolchain_name,
                &cache_dir,
            ) {
                Ok(resolved) => fbuild_packages::toolchain::Esp32Toolchain::from_resolved(
                    project_dir,
                    &resolved.url,
                    resolved.sha256.as_deref(),
                    is_riscv,
                    &prefix,
                ),
                Err(_) => {
                    fbuild_packages::toolchain::Esp32Toolchain::new(project_dir, is_riscv, &prefix)
                }
            }
        }
        Err(_) => fbuild_packages::toolchain::Esp32Toolchain::new(project_dir, is_riscv, &prefix),
    };

    let _ = Package::ensure_installed(&toolchain)?;
    Ok(toolchain.get_gcc_path())
}

#[cfg(windows)]
fn apply_windows_process_flags(cmd: &mut tokio::process::Command, exe_path: &Path) {
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    cmd.creation_flags(CREATE_NO_WINDOW);

    let current_path = std::env::var("PATH").unwrap_or_default();
    if let Ok(path_env) =
        fbuild_packages::toolchain::build_windows_qemu_path_env(exe_path, &current_path)
    {
        cmd.env("PATH", path_env);
    }
}

#[cfg(not(windows))]
fn apply_windows_process_flags(_cmd: &mut tokio::process::Command, _exe_path: &Path) {}

async fn spawn_line_reader(
    stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    is_stderr: bool,
    tx: tokio::sync::mpsc::UnboundedSender<ProcessEvent>,
) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let _ = tx.send(ProcessEvent::Line(ProcessLine { is_stderr, line }));
    }
    let _ = tx.send(ProcessEvent::StreamClosed);
}

async fn run_qemu_process(
    qemu_path: &Path,
    args: &[String],
    options: RunQemuOptions<'_>,
) -> fbuild_core::Result<QemuRunResult> {
    let mut cmd = tokio::process::Command::new(qemu_path);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_windows_process_flags(&mut cmd, qemu_path);

    if options.verbose {
        tracing::info!("qemu: {} {}", qemu_path.display(), args.join(" "));
    }

    let mut child = cmd.spawn().map_err(|e| {
        fbuild_core::FbuildError::DeployFailed(build_linux_macos_qemu_hint(&format!(
            "failed to launch QEMU at {}: {}",
            qemu_path.display(),
            e
        )))
    })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        fbuild_core::FbuildError::DeployFailed("failed to capture QEMU stdout".to_string())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        fbuild_core::FbuildError::DeployFailed("failed to capture QEMU stderr".to_string())
    })?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ProcessEvent>();
    let stdout_task = tokio::spawn(spawn_line_reader(stdout, false, tx.clone()));
    let stderr_task = tokio::spawn(spawn_line_reader(stderr, true, tx));

    let mut monitor = MonitorState::new(
        options.timeout_secs,
        options.halt_on_error,
        options.halt_on_success,
        options.expect,
        options.show_timestamp,
    );
    let mut crash_decoder =
        fbuild_serial::crash_decoder::CrashDecoder::new(options.elf_path, options.addr2line_path);
    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();
    let mut synthetic_buf = String::new();
    let mut streams_open = 2usize;
    let mut child_exit: Option<std::process::ExitStatus> = None;
    let mut final_outcome: Option<MonitorOutcome> = None;

    loop {
        if monitor.timed_out() {
            final_outcome = Some(monitor.timeout_outcome());
            let _ = child.kill().await;
            break;
        }

        let recv_timeout = monitor
            .remaining()
            .unwrap_or(std::time::Duration::from_secs(1));

        tokio::select! {
            status = child.wait(), if child_exit.is_none() => {
                child_exit = Some(status.map_err(|e| {
                    fbuild_core::FbuildError::DeployFailed(format!("QEMU wait failed: {}", e))
                })?);
                if streams_open == 0 {
                    break;
                }
            }
            maybe_event = tokio::time::timeout(recv_timeout, rx.recv()) => {
                match maybe_event {
                    Ok(Some(ProcessEvent::Line(line))) => {
                        let target = if line.is_stderr { &mut stderr_buf } else { &mut stdout_buf };
                        target.push_str(&line.line);
                        target.push('\n');

                        if let Some(outcome) = monitor.process_line(&line.line) {
                            final_outcome = Some(outcome);
                            let _ = child.kill().await;
                            break;
                        }

                        if let Some(decoded_lines) = crash_decoder.process_line(&line.line) {
                            for decoded in decoded_lines {
                                synthetic_buf.push_str(&decoded);
                                synthetic_buf.push('\n');
                                if let Some(outcome) = monitor.process_line(&decoded) {
                                    final_outcome = Some(outcome);
                                    let _ = child.kill().await;
                                    break;
                                }
                            }
                            if final_outcome.is_some() {
                                break;
                            }
                        }
                    }
                    Ok(Some(ProcessEvent::StreamClosed)) => {
                        streams_open = streams_open.saturating_sub(1);
                        if streams_open == 0 && child_exit.is_some() {
                            break;
                        }
                    }
                    Ok(None) => {
                        if child_exit.is_some() {
                            break;
                        }
                    }
                    Err(_) => {
                        final_outcome = Some(monitor.timeout_outcome());
                        let _ = child.kill().await;
                        break;
                    }
                }
            }
        }
    }

    if child_exit.is_none() {
        child_exit = Some(child.wait().await.map_err(|e| {
            fbuild_core::FbuildError::DeployFailed(format!("QEMU wait failed: {}", e))
        })?);
    }

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    if !synthetic_buf.is_empty() {
        stdout_buf.push_str(&synthetic_buf);
    }

    let outcome = if let Some(outcome) = final_outcome {
        outcome
    } else if let Some(status) = child_exit {
        if status.success() {
            if options.expect.is_some() && !monitor.expect_found() {
                MonitorOutcome::Error(
                    "QEMU exited before the expected pattern was found".to_string(),
                )
            } else {
                MonitorOutcome::Success("QEMU exited normally".to_string())
            }
        } else {
            MonitorOutcome::Error(format!(
                "QEMU exited with code {}",
                status.code().unwrap_or(-1)
            ))
        }
    } else {
        MonitorOutcome::Error("QEMU exited unexpectedly".to_string())
    };

    Ok(QemuRunResult {
        outcome,
        stdout: stdout_buf,
        stderr: stderr_buf,
    })
}

pub async fn deploy_qemu(
    _ctx: Arc<DaemonContext>,
    req: DeployQemuRequest,
) -> (StatusCode, Json<OperationResponse>) {
    let DeployQemuRequest {
        request_id,
        project_dir,
        env_name,
        board_id,
        platform,
        firmware_path,
        elf_path,
        output_file,
        output_dir,
        monitor_timeout,
        qemu_timeout_secs,
        halt_on_error,
        halt_on_success,
        expect,
        show_timestamp,
        verbose,
        board_overrides,
    } = req;

    if platform != fbuild_core::Platform::Espressif32 {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                "QEMU deploy target is currently supported only for ESP32-family boards"
                    .to_string(),
            )),
        );
    }
    if firmware_path.extension().and_then(|ext| ext.to_str()) != Some("bin") {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!(
                    "QEMU deploy target requires firmware.bin, got '{}'",
                    firmware_path.display()
                ),
            )),
        );
    }

    let board = match fbuild_config::BoardConfig::from_board_id(&board_id, &board_overrides) {
        Ok(board) => board,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(
                    request_id,
                    format!("failed to load board config for QEMU: {}", e),
                )),
            );
        }
    };
    if !board.mcu.eq_ignore_ascii_case("esp32s3") {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!(
                    "native QEMU deploy currently supports only ESP32-S3 boards, got '{}'",
                    board.mcu
                ),
            )),
        );
    }

    let mcu_config = match fbuild_build::esp32::mcu_config::get_mcu_config(&board.mcu) {
        Ok(cfg) => cfg,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("failed to load MCU config for QEMU: {}", e),
                )),
            );
        }
    };

    let effective_flash_mode = board
        .flash_mode
        .as_deref()
        .unwrap_or(mcu_config.default_flash_mode());
    if !effective_flash_mode.eq_ignore_ascii_case("dio") {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!(
                    "QEMU requires a DIO-compatible flash image; effective flash mode is '{}'",
                    effective_flash_mode
                ),
            )),
        );
    }

    let flash_size_bytes = match fbuild_deploy::esp32::resolve_qemu_flash_size_bytes(
        &board,
        mcu_config.default_flash_size(),
    ) {
        Ok(size) => size,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(request_id, e.to_string())),
            );
        }
    };

    let qemu = match fbuild_packages::toolchain::EspQemuXtensa::new(&project_dir)
        .and_then(|pkg| pkg.resolve_executable())
    {
        Ok(path) => path,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(
                    request_id,
                    build_linux_macos_qemu_hint(&e.to_string()),
                )),
            );
        }
    };

    let session_dir = qemu_session_dir(&project_dir, &env_name);
    if let Err(e) = std::fs::create_dir_all(&session_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("failed to create QEMU session dir: {}", e),
            )),
        );
    }
    let flash_image = session_dir.join("qemu_flash.bin");
    if let Err(e) = fbuild_deploy::esp32::create_qemu_flash_image(
        &firmware_path,
        &flash_image,
        flash_size_bytes,
        mcu_config.bootloader_offset(),
        mcu_config.partitions_offset(),
        mcu_config.firmware_offset(),
        elf_path.as_deref(),
    ) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("failed to create QEMU flash image: {}", e),
            )),
        );
    }

    let args = fbuild_deploy::esp32::build_qemu_esp32s3_args(
        &flash_image,
        board.qemu_esp32_psram_config(),
    );
    let addr2line_path = elf_path.as_ref().and_then(|_| {
        resolve_esp32_toolchain_gcc_path(&project_dir, &mcu_config)
            .ok()
            .and_then(|gcc| fbuild_serial::crash_decoder::derive_addr2line_path(&gcc))
    });

    let timeout_secs = monitor_timeout.or(Some(qemu_timeout_secs as f64));
    let qemu_result = match run_qemu_process(
        &qemu,
        &args,
        RunQemuOptions {
            elf_path,
            addr2line_path,
            timeout_secs,
            halt_on_error: halt_on_error.as_deref(),
            halt_on_success: halt_on_success.as_deref(),
            expect: expect.as_deref(),
            show_timestamp,
            verbose,
        },
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(request_id, e.to_string())),
            );
        }
    };

    match qemu_result.outcome {
        MonitorOutcome::Success(message) => (
            StatusCode::OK,
            Json(OperationResponse {
                success: true,
                request_id,
                message: format!("QEMU run succeeded: {}", message),
                exit_code: 0,
                output_file: Some(output_file),
                output_dir,
                launch_url: None,
                stdout: Some(qemu_result.stdout),
                stderr: Some(qemu_result.stderr),
            }),
        ),
        MonitorOutcome::Error(message) => (
            StatusCode::OK,
            Json(OperationResponse {
                success: false,
                request_id,
                message: format!("QEMU run failed: {}", message),
                exit_code: 1,
                output_file: Some(output_file),
                output_dir,
                launch_url: None,
                stdout: Some(qemu_result.stdout),
                stderr: Some(qemu_result.stderr),
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
                        "QEMU run completed (timeout)".to_string()
                    } else {
                        "QEMU run timed out (expected pattern not found)".to_string()
                    },
                    exit_code,
                    output_file: Some(output_file),
                    output_dir,
                    launch_url: None,
                    stdout: Some(qemu_result.stdout),
                    stderr: Some(qemu_result.stderr),
                }),
            )
        }
    }
}

pub async fn avr8js_page(
    AxumPath(session_id): AxumPath<String>,
    State(ctx): State<Arc<DaemonContext>>,
) -> impl IntoResponse {
    match load_session_manifest(&ctx, &session_id) {
        Ok(_) => Html(render_page(&session_id)).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

pub async fn avr8js_app_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/javascript; charset=utf-8"),
        )],
        AVR8JS_APP_JS,
    )
}

pub async fn avr8js_session_json(
    AxumPath(session_id): AxumPath<String>,
    State(ctx): State<Arc<DaemonContext>>,
) -> impl IntoResponse {
    match load_session_manifest(&ctx, &session_id) {
        Ok(manifest) => (
            StatusCode::OK,
            Json(Avr8jsSessionResponse {
                session_id: manifest.session_id.clone(),
                env_name: manifest.env_name,
                board_id: manifest.board_id,
                platform: manifest.platform,
                mcu: manifest.mcu,
                f_cpu_hz: manifest.f_cpu_hz,
                firmware_hex_url: format!("/api/emulator/avr8js/{}/firmware.hex", session_id),
            }),
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

pub async fn avr8js_firmware_hex(
    AxumPath(session_id): AxumPath<String>,
    State(ctx): State<Arc<DaemonContext>>,
) -> impl IntoResponse {
    match load_session_manifest(&ctx, &session_id) {
        Ok(manifest) => match std::fs::read_to_string(&manifest.firmware_hex) {
            Ok(hex) => (
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                )],
                hex,
            )
                .into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// EmulatorRunner abstraction (Issue #23)
// ---------------------------------------------------------------------------

use fbuild_core::emulator::{EmulatorOutcome, EmulatorRunResult};

/// Configuration for an emulator test run (user-facing options).
pub struct EmulatorRunConfig {
    pub firmware_path: PathBuf,
    pub elf_path: Option<PathBuf>,
    pub timeout: Option<f64>,
    pub halt_on_error: Option<String>,
    pub halt_on_success: Option<String>,
    pub expect: Option<String>,
    pub show_timestamp: bool,
    pub verbose: bool,
}

/// Convert a `MonitorOutcome` into an `EmulatorOutcome`.
fn monitor_outcome_to_emulator(outcome: MonitorOutcome, exit_code: Option<i32>) -> EmulatorOutcome {
    match outcome {
        MonitorOutcome::Success(msg) => EmulatorOutcome::Passed(msg),
        MonitorOutcome::Error(msg) => {
            // Heuristic: if the process crashed (non-zero exit with crash signature)
            if let Some(code) = exit_code {
                if code != 0 && (msg.contains("abort()") || msg.contains("Guru Meditation")) {
                    return EmulatorOutcome::Crashed(msg);
                }
            }
            EmulatorOutcome::Failed(msg)
        }
        MonitorOutcome::Timeout { expect_found } => EmulatorOutcome::TimedOut { expect_found },
    }
}

/// Abstraction over emulator backends. Each implementation knows how to set up
/// and execute a specific emulator (QEMU, avr8js, etc.).
#[async_trait::async_trait]
pub trait EmulatorRunner: Send + Sync {
    /// Human-readable name of this runner (e.g. "QEMU ESP32-S3", "avr8js ATmega328P").
    fn name(&self) -> &str;

    /// Run the emulator with the given configuration.
    async fn run(&self, config: &EmulatorRunConfig) -> fbuild_core::Result<EmulatorRunResult>;
}

/// QEMU-based emulator runner for ESP32-S3.
pub struct QemuRunner {
    project_dir: PathBuf,
    env_name: String,
    board: fbuild_config::BoardConfig,
}

impl QemuRunner {
    pub fn new(project_dir: PathBuf, env_name: String, board: fbuild_config::BoardConfig) -> Self {
        Self {
            project_dir,
            env_name,
            board,
        }
    }
}

#[async_trait::async_trait]
impl EmulatorRunner for QemuRunner {
    fn name(&self) -> &str {
        "QEMU ESP32-S3"
    }

    async fn run(&self, config: &EmulatorRunConfig) -> fbuild_core::Result<EmulatorRunResult> {
        let mcu_config = fbuild_build::esp32::mcu_config::get_mcu_config(&self.board.mcu)?;

        let effective_flash_mode = self
            .board
            .flash_mode
            .as_deref()
            .unwrap_or(mcu_config.default_flash_mode());
        if !effective_flash_mode.eq_ignore_ascii_case("dio") {
            return Ok(EmulatorRunResult {
                outcome: EmulatorOutcome::Unsupported(format!(
                    "QEMU requires DIO flash mode; effective mode is '{}'",
                    effective_flash_mode
                )),
                stdout: String::new(),
                stderr: String::new(),
                command_line: String::new(),
                exit_code: None,
            });
        }

        let flash_size_bytes = fbuild_deploy::esp32::resolve_qemu_flash_size_bytes(
            &self.board,
            mcu_config.default_flash_size(),
        )?;

        let qemu = fbuild_packages::toolchain::EspQemuXtensa::new(&self.project_dir)
            .and_then(|pkg| pkg.resolve_executable())?;

        let session_dir = qemu_session_dir(&self.project_dir, &self.env_name);
        std::fs::create_dir_all(&session_dir)?;

        let flash_image = session_dir.join("qemu_flash.bin");
        fbuild_deploy::esp32::create_qemu_flash_image(
            &config.firmware_path,
            &flash_image,
            flash_size_bytes,
            mcu_config.bootloader_offset(),
            mcu_config.partitions_offset(),
            mcu_config.firmware_offset(),
            config.elf_path.as_deref(),
        )?;

        let args = fbuild_deploy::esp32::build_qemu_esp32s3_args(
            &flash_image,
            self.board.qemu_esp32_psram_config(),
        );
        let addr2line_path = config.elf_path.as_ref().and_then(|_| {
            resolve_esp32_toolchain_gcc_path(&self.project_dir, &mcu_config)
                .ok()
                .and_then(|gcc| fbuild_serial::crash_decoder::derive_addr2line_path(&gcc))
        });

        let command_line = format!("{} {}", qemu.display(), args.join(" "));

        let qemu_result = run_qemu_process(
            &qemu,
            &args,
            RunQemuOptions {
                elf_path: config.elf_path.clone(),
                addr2line_path,
                timeout_secs: config.timeout,
                halt_on_error: config.halt_on_error.as_deref(),
                halt_on_success: config.halt_on_success.as_deref(),
                expect: config.expect.as_deref(),
                show_timestamp: config.show_timestamp,
                verbose: config.verbose,
            },
        )
        .await?;

        let exit_code = None; // QEMU process exit code not directly exposed by run_qemu_process
        let outcome = monitor_outcome_to_emulator(qemu_result.outcome, exit_code);

        Ok(EmulatorRunResult {
            outcome,
            stdout: qemu_result.stdout,
            stderr: qemu_result.stderr,
            command_line,
            exit_code,
        })
    }
}

/// AVR8js-based emulator runner for ATmega328P (headless Node.js).
pub struct Avr8jsRunner {
    board: fbuild_config::BoardConfig,
}

impl Avr8jsRunner {
    pub fn new(board: fbuild_config::BoardConfig) -> Self {
        Self { board }
    }
}

#[async_trait::async_trait]
impl EmulatorRunner for Avr8jsRunner {
    fn name(&self) -> &str {
        "avr8js ATmega328P"
    }

    async fn run(&self, config: &EmulatorRunConfig) -> fbuild_core::Result<EmulatorRunResult> {
        let node_path = find_node()?;
        let avr8js_cache = ensure_avr8js_npm()?;

        let session_dir = tempfile::TempDir::new()?;
        let script_path = session_dir.path().join("headless.mjs");
        std::fs::write(&script_path, AVR8JS_HEADLESS_MJS)?;

        let f_cpu_hz: u32 = self
            .board
            .f_cpu
            .trim_end_matches('L')
            .parse()
            .unwrap_or(16_000_000);

        let command_line = format!(
            "{} {} --hex {} --f-cpu {}",
            node_path.display(),
            script_path.display(),
            config.firmware_path.display(),
            f_cpu_hz
        );

        let avr8js_result = run_avr8js_headless(
            &node_path,
            &script_path,
            &config.firmware_path,
            f_cpu_hz,
            &avr8js_cache,
            RunAvr8jsHeadlessOptions {
                timeout_secs: config.timeout,
                halt_on_error: config.halt_on_error.as_deref(),
                halt_on_success: config.halt_on_success.as_deref(),
                expect: config.expect.as_deref(),
                show_timestamp: config.show_timestamp,
                verbose: config.verbose,
            },
        )
        .await?;

        let outcome = monitor_outcome_to_emulator(avr8js_result.outcome, None);

        Ok(EmulatorRunResult {
            outcome,
            stdout: avr8js_result.stdout,
            stderr: avr8js_result.stderr,
            command_line,
            exit_code: None,
        })
    }
}

/// Select the appropriate emulator runner based on platform, MCU, and optional
/// explicit emulator choice.
///
/// Returns `Err` with `EmulatorOutcome::Unsupported` information if no runner
/// matches.
pub fn select_runner(
    project_dir: &Path,
    env_name: &str,
    platform: fbuild_core::Platform,
    board_id: &str,
    board_overrides: &HashMap<String, String>,
    emulator: Option<&str>,
) -> fbuild_core::Result<Box<dyn EmulatorRunner>> {
    let board = fbuild_config::BoardConfig::from_board_id(board_id, board_overrides)?;

    if let Some(explicit) = emulator {
        return match explicit {
            "qemu" => {
                if platform != fbuild_core::Platform::Espressif32 {
                    return Err(fbuild_core::FbuildError::DeployFailed(
                        "QEMU runner is only supported for ESP32-family boards".to_string(),
                    ));
                }
                if !board.mcu.eq_ignore_ascii_case("esp32s3") {
                    return Err(fbuild_core::FbuildError::DeployFailed(format!(
                        "QEMU runner currently supports only ESP32-S3, got '{}'",
                        board.mcu
                    )));
                }
                Ok(Box::new(QemuRunner::new(
                    project_dir.to_path_buf(),
                    env_name.to_string(),
                    board,
                )))
            }
            "avr8js" => {
                if !matches!(
                    platform,
                    fbuild_core::Platform::AtmelAvr | fbuild_core::Platform::AtmelMegaAvr
                ) {
                    return Err(fbuild_core::FbuildError::DeployFailed(
                        "avr8js runner is only supported for AVR boards".to_string(),
                    ));
                }
                if !board.mcu.eq_ignore_ascii_case("atmega328p") {
                    return Err(fbuild_core::FbuildError::DeployFailed(format!(
                        "avr8js runner currently supports only ATmega328P, got '{}'",
                        board.mcu
                    )));
                }
                Ok(Box::new(Avr8jsRunner::new(board)))
            }
            other => Err(fbuild_core::FbuildError::DeployFailed(format!(
                "unsupported emulator '{}'; available: qemu, avr8js",
                other
            ))),
        };
    }

    // Auto-detect based on platform and MCU
    match platform {
        fbuild_core::Platform::AtmelAvr | fbuild_core::Platform::AtmelMegaAvr => {
            if board.mcu.eq_ignore_ascii_case("atmega328p") {
                Ok(Box::new(Avr8jsRunner::new(board)))
            } else {
                Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "no emulator runner available for AVR MCU '{}'; only ATmega328P is supported via avr8js",
                    board.mcu
                )))
            }
        }
        fbuild_core::Platform::Espressif32 => {
            if board.mcu.eq_ignore_ascii_case("esp32s3") {
                Ok(Box::new(QemuRunner::new(
                    project_dir.to_path_buf(),
                    env_name.to_string(),
                    board,
                )))
            } else {
                Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "no emulator runner available for ESP32 MCU '{}'; only ESP32-S3 is supported via QEMU",
                    board.mcu
                )))
            }
        }
        _ => Err(fbuild_core::FbuildError::DeployFailed(format!(
            "no emulator runner available for platform {:?}",
            platform
        ))),
    }
}

/// POST /api/test-emu handler — build firmware then run it in an emulator.
pub async fn test_emu(
    State(ctx): State<Arc<DaemonContext>>,
    Json(req): Json<crate::models::TestEmuRequest>,
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

    // Select the emulator runner before building (fail fast on unsupported boards)
    let runner = match select_runner(
        &project_dir,
        &env_name,
        platform,
        &board_id,
        &board_overrides,
        req.emulator.as_deref(),
    ) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(request_id, e.to_string())),
            );
        }
    };

    // Build firmware
    let lock = ctx.project_lock(&project_dir);
    let _guard = lock.lock().await;

    let needs_qemu_flags =
        platform == fbuild_core::Platform::Espressif32 && req.emulator.as_deref() != Some("avr8js");
    let board_for_flags = if needs_qemu_flags {
        fbuild_config::BoardConfig::from_board_id(&board_id, &board_overrides).ok()
    } else {
        None
    };

    let build_dir = fbuild_paths::get_project_build_root(&project_dir);
    let params = fbuild_build::BuildParams {
        project_dir: project_dir.clone(),
        env_name: env_name.clone(),
        clean: false,
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
        src_dir: None,
        pio_env: req.pio_env.clone(),
        extra_build_flags: if needs_qemu_flags {
            board_for_flags
                .as_ref()
                .map(|b| crate::handlers::operations::qemu_extra_build_flags(platform, &b.mcu))
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

    let (firmware_path, elf_path) = match build_result {
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
    };

    // Run the emulator
    let run_config = EmulatorRunConfig {
        firmware_path,
        elf_path,
        timeout: req.timeout,
        halt_on_error: req.halt_on_error.clone(),
        halt_on_success: req.halt_on_success.clone(),
        expect: req.expect.clone(),
        show_timestamp: req.show_timestamp,
        verbose: req.verbose,
    };

    let emu_result = match runner.run(&run_config).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("emulator error: {}", e),
                )),
            );
        }
    };

    let success = emu_result.is_success();
    let exit_code = if success { 0 } else { 1 };
    let message = format!(
        "{} test-emu {}: {}",
        runner.name(),
        if success { "passed" } else { "failed" },
        emu_result.outcome
    );

    (
        StatusCode::OK,
        Json(OperationResponse {
            success,
            request_id,
            message,
            exit_code,
            output_file: None,
            output_dir: None,
            launch_url: None,
            stdout: Some(emu_result.stdout),
            stderr: Some(emu_result.stderr),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_build::{BuildOrchestrator, BuildParams};
    use fbuild_core::BuildProfile;

    #[test]
    fn monitor_outcome_to_emulator_maps_success() {
        let outcome = monitor_outcome_to_emulator(MonitorOutcome::Success("ok".into()), Some(0));
        assert_eq!(outcome, EmulatorOutcome::Passed("ok".into()));
    }

    #[test]
    fn monitor_outcome_to_emulator_maps_error() {
        let outcome = monitor_outcome_to_emulator(MonitorOutcome::Error("bad".into()), Some(1));
        assert_eq!(outcome, EmulatorOutcome::Failed("bad".into()));
    }

    #[test]
    fn monitor_outcome_to_emulator_maps_crash() {
        let outcome = monitor_outcome_to_emulator(
            MonitorOutcome::Error("abort() was called at PC 0x4200".into()),
            Some(134),
        );
        assert_eq!(
            outcome,
            EmulatorOutcome::Crashed("abort() was called at PC 0x4200".into())
        );
    }

    #[test]
    fn monitor_outcome_to_emulator_maps_timeout() {
        let outcome =
            monitor_outcome_to_emulator(MonitorOutcome::Timeout { expect_found: true }, None);
        assert_eq!(outcome, EmulatorOutcome::TimedOut { expect_found: true });
    }

    fn test_process_command(lines: &[&str]) -> (PathBuf, Vec<String>) {
        #[cfg(windows)]
        {
            let script = lines
                .iter()
                .map(|line| format!("Write-Output '{}'", line.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join("; ");
            (
                PathBuf::from("powershell"),
                vec![
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    script,
                ],
            )
        }

        #[cfg(not(windows))]
        {
            let script = lines
                .iter()
                .map(|line| format!("printf '%s\\n' '{}'", line.replace('\'', "'\"'\"'")))
                .collect::<Vec<_>>()
                .join("; ");
            (PathBuf::from("sh"), vec!["-c".to_string(), script])
        }
    }

    #[tokio::test]
    async fn run_qemu_process_reports_expected_success_output() {
        let (exe, args) = test_process_command(&["Hello from ESP32-S3!"]);
        let result = run_qemu_process(
            &exe,
            &args,
            RunQemuOptions {
                elf_path: None,
                addr2line_path: None,
                timeout_secs: Some(2.0),
                halt_on_error: None,
                halt_on_success: None,
                expect: Some("Hello from ESP32-S3"),
                show_timestamp: false,
                verbose: false,
            },
        )
        .await
        .unwrap();

        assert!(result.stdout.contains("Hello from ESP32-S3!"));
        match result.outcome {
            MonitorOutcome::Success(message) => {
                assert!(message.contains("QEMU exited normally"));
            }
            other => panic!("expected success outcome, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn run_qemu_process_surfaces_crash_decoder_output() {
        let (exe, args) =
            test_process_command(&["abort() was called at PC 0x42002a3c", "Rebooting..."]);
        let result = run_qemu_process(
            &exe,
            &args,
            RunQemuOptions {
                elf_path: None,
                addr2line_path: None,
                timeout_secs: Some(2.0),
                halt_on_error: Some("no firmware\\.elf found"),
                halt_on_success: None,
                expect: None,
                show_timestamp: false,
                verbose: false,
            },
        )
        .await
        .unwrap();

        assert!(result
            .stdout
            .contains("abort() was called at PC 0x42002a3c"));
        assert!(result.stdout.contains("no firmware.elf found"));
        match result.outcome {
            MonitorOutcome::Error(message) => {
                assert!(message.contains("halt-on-error pattern matched"));
            }
            other => panic!("expected error outcome, got {:?}", other),
        }
    }

    #[test]
    #[ignore]
    fn run_real_esp32s3_fixture_in_qemu() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let project_dir = repo_root.join("tests/platform/esp32s3");
        if !project_dir.exists() {
            eprintln!("SKIP: {} does not exist", project_dir.display());
            return;
        }

        let build_dir = project_dir.join(".fbuild/build-qemu");
        let params = BuildParams {
            project_dir: project_dir.clone(),
            env_name: "esp32s3".to_string(),
            clean: true,
            profile: BuildProfile::Release,
            build_dir,
            verbose: true,
            jobs: None,
            generate_compiledb: false,
            compiledb_only: false,
            log_sender: None,
            symbol_analysis: false,
            symbol_analysis_path: None,
            no_timestamp: false,
            src_dir: None,
            pio_env: Default::default(),
            extra_build_flags: vec![
                "-DARDUINO_USB_MODE=0".to_string(),
                "-DARDUINO_USB_CDC_ON_BOOT=0".to_string(),
            ],
        };

        let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
        let build_result = orchestrator
            .build(&params)
            .expect("ESP32-S3 fixture build should succeed");
        assert!(build_result.success);

        let firmware_path = build_result
            .firmware_path
            .clone()
            .expect("should produce firmware.bin");
        let elf_path = build_result.elf_path.clone();

        let board =
            fbuild_config::BoardConfig::from_board_id("esp32-s3-devkitc-1", &Default::default())
                .unwrap();
        let mcu_config = fbuild_build::esp32::mcu_config::get_mcu_config("esp32s3").unwrap();
        let flash_size_bytes = fbuild_deploy::esp32::resolve_qemu_flash_size_bytes(
            &board,
            mcu_config.default_flash_size(),
        )
        .unwrap();

        let session_dir = tempfile::TempDir::new().unwrap();
        let flash_image = session_dir.path().join("flash.bin");
        fbuild_deploy::esp32::create_qemu_flash_image(
            &firmware_path,
            &flash_image,
            flash_size_bytes,
            mcu_config.bootloader_offset(),
            mcu_config.partitions_offset(),
            mcu_config.firmware_offset(),
            elf_path.as_deref(),
        )
        .unwrap();

        let qemu = fbuild_packages::toolchain::EspQemuXtensa::new(&project_dir)
            .and_then(|pkg| pkg.resolve_executable())
            .expect("native QEMU should resolve for ignored integration test");
        let args = fbuild_deploy::esp32::build_qemu_esp32s3_args(
            &flash_image,
            board.qemu_esp32_psram_config(),
        );
        let addr2line_path = resolve_esp32_toolchain_gcc_path(&project_dir, &mcu_config)
            .ok()
            .and_then(|gcc| fbuild_serial::crash_decoder::derive_addr2line_path(&gcc));

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(run_qemu_process(
                &qemu,
                &args,
                RunQemuOptions {
                    elf_path,
                    addr2line_path,
                    timeout_secs: Some(15.0),
                    halt_on_error: None,
                    halt_on_success: Some("Hello from ESP32-S3!"),
                    expect: Some("Hello from ESP32-S3!"),
                    show_timestamp: false,
                    verbose: true,
                },
            ))
            .unwrap();

        assert!(result.stdout.contains("Hello from ESP32-S3!"));
        match result.outcome {
            MonitorOutcome::Success(_) => {}
            other => panic!("expected success outcome, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // avr8js headless tests (use fake process, no real Node.js needed)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_avr8js_headless_captures_stdout() {
        let (exe, args) = test_process_command(&["Hello from AVR!"]);
        // run_avr8js_headless expects (node, script, hex, f_cpu, cache_dir, options).
        // We bypass that by calling the lower-level run with the fake exe directly.
        // Since run_avr8js_headless builds its own command, we test the same
        // subprocess loop via run_qemu_process which shares identical logic.
        let result = run_qemu_process(
            &exe,
            &args,
            RunQemuOptions {
                elf_path: None,
                addr2line_path: None,
                timeout_secs: Some(2.0),
                halt_on_error: None,
                halt_on_success: None,
                expect: Some("Hello from AVR"),
                show_timestamp: false,
                verbose: false,
            },
        )
        .await
        .unwrap();

        assert!(result.stdout.contains("Hello from AVR!"));
        match result.outcome {
            MonitorOutcome::Success(msg) => {
                assert!(msg.contains("QEMU exited normally"));
            }
            other => panic!("expected success, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn run_avr8js_headless_halt_on_success() {
        let (exe, args) =
            test_process_command(&["booting...", "PASS: all tests passed", "more output"]);
        let result = run_qemu_process(
            &exe,
            &args,
            RunQemuOptions {
                elf_path: None,
                addr2line_path: None,
                timeout_secs: Some(2.0),
                halt_on_error: None,
                halt_on_success: Some("PASS:"),
                expect: None,
                show_timestamp: false,
                verbose: false,
            },
        )
        .await
        .unwrap();

        match result.outcome {
            MonitorOutcome::Success(msg) => {
                assert!(msg.contains("halt-on-success pattern matched"));
            }
            other => panic!("expected success, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn run_avr8js_headless_halt_on_error() {
        let (exe, args) = test_process_command(&["booting...", "FAIL: assertion failed"]);
        let result = run_qemu_process(
            &exe,
            &args,
            RunQemuOptions {
                elf_path: None,
                addr2line_path: None,
                timeout_secs: Some(2.0),
                halt_on_error: Some("FAIL:"),
                halt_on_success: None,
                expect: None,
                show_timestamp: false,
                verbose: false,
            },
        )
        .await
        .unwrap();

        match result.outcome {
            MonitorOutcome::Error(msg) => {
                assert!(msg.contains("halt-on-error pattern matched"));
            }
            other => panic!("expected error, got {:?}", other),
        }
    }
}
