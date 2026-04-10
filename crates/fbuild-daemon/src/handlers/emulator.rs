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

    let launch_url = if monitor_after {
        Some(format!(
            "http://127.0.0.1:{}/emulator/avr8js/{}",
            ctx.port, session_id
        ))
    } else {
        None
    };

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

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_build::{BuildOrchestrator, BuildParams};
    use fbuild_core::BuildProfile;

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
}
