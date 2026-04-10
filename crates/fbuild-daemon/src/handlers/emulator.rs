use crate::context::DaemonContext;
use crate::models::OperationResponse;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

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

pub async fn avr8js_page(
    Path(session_id): Path<String>,
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
    Path(session_id): Path<String>,
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
    Path(session_id): Path<String>,
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
