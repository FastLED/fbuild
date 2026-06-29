//! AVR8js browser-side web UI handlers and session manifest types.
//!
//! Serves the static HTML shell, the `app.js` ES module, the per-session
//! `session.json`, and the raw firmware hex bytes for the in-browser AVR8js
//! emulator. The session manifest is registered into [`DaemonContext`] from
//! [`super::avr8js_deploy::deploy_avr8js`].

use crate::context::DaemonContext;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const AVR8JS_APP_JS: &str = include_str!("../../../web/avr8js/app.js");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Avr8jsSessionManifest {
    pub session_id: String,
    pub project_dir: String,
    pub env_name: String,
    pub board_id: String,
    pub platform: String,
    pub mcu: String,
    pub f_cpu_hz: u32,
    pub firmware_hex: String,
    pub firmware_elf: Option<String>,
    pub created_at_unix: f64,
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

pub(crate) fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub(crate) fn render_page(session_id: &str) -> String {
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

pub(crate) async fn load_session_manifest(
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
    let raw = tokio::fs::read_to_string(&manifest_path).await?;
    serde_json::from_str(&raw).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to parse AVR8js session manifest: {}", e))
    })
}

pub async fn avr8js_page(
    AxumPath(session_id): AxumPath<String>,
    State(ctx): State<Arc<DaemonContext>>,
) -> impl IntoResponse {
    match load_session_manifest(&ctx, &session_id).await {
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
    match load_session_manifest(&ctx, &session_id).await {
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
    match load_session_manifest(&ctx, &session_id).await {
        Ok(manifest) => match tokio::fs::read_to_string(&manifest.firmware_hex).await {
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
