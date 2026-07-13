//! Shared `OperationOutcome` / `OpRequest` types plus the sync and async
//! HTTP transports used by `DaemonConnection` and `AsyncDaemonConnection`.

use pyo3::prelude::*;
use serde::Serialize;

#[derive(Clone, Serialize)]
pub(crate) struct OpRequest {
    pub(crate) project_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) environment: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) clean_build: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) verbose: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) port: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) monitor_after: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) skip_build: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) baud_rate: Option<u32>,
    /// Override for `PLATFORMIO_SRC_DIR` — the source directory to compile.
    ///
    /// Mirrors the `fbuild-cli` build/deploy paths which read the env var at
    /// request-construction time and forward it to the daemon, so consumers
    /// that go through the PyO3 binding (notably FastLED's autoresearch
    /// runner) get the same `src_dir` override the CLI provides. The
    /// daemon's `BuildRequest`/`DeployRequest` both honor a top-level
    /// `src_dir` field. See FastLED/fbuild#274.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) src_dir: Option<String>,
}

/// Read `PLATFORMIO_SRC_DIR` from the current process env, returning `None`
/// when unset or empty.
///
/// Centralized so the sync and async `DaemonConnection`s populate
/// `OpRequest.src_dir` identically to `fbuild-cli`
/// (`std::env::var(...).ok().filter(|s| !s.is_empty())`). Keeping this in
/// one place avoids drift between the two surfaces and gives tests a single
/// seam to exercise.
pub(crate) fn platformio_src_dir_from_env() -> Option<String> {
    std::env::var("PLATFORMIO_SRC_DIR")
        .ok()
        .filter(|s| !s.is_empty())
}

pub(crate) fn build_url() -> String {
    format!("{}/api/build", fbuild_paths::get_daemon_url())
}

pub(crate) fn deploy_url() -> String {
    format!("{}/api/deploy", fbuild_paths::get_daemon_url())
}

pub(crate) fn monitor_url() -> String {
    format!("{}/api/monitor", fbuild_paths::get_daemon_url())
}

/// Structured result of a daemon operation (build/deploy/monitor).
///
/// Used internally by `send_op` and exposed to Python callers via
/// `DaemonConnection::{build,deploy,monitor}_result`. Lets callers branch
/// on specific failure modes (transport error vs. build error vs. no
/// response) instead of inspecting a bare bool. See FastLED/fbuild#18.
#[derive(Debug, Clone, Default)]
pub(crate) struct OperationOutcome {
    pub(crate) success: bool,
    pub(crate) message: Option<String>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: Option<String>,
    pub(crate) stderr: Option<String>,
}

pub(crate) fn outcome_to_pydict<'py>(
    py: Python<'py>,
    outcome: &OperationOutcome,
) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
    let dict = pyo3::types::PyDict::new(py);
    dict.set_item("success", outcome.success)?;
    dict.set_item("message", outcome.message.clone())?;
    dict.set_item("exit_code", outcome.exit_code)?;
    dict.set_item("stdout", outcome.stdout.clone())?;
    dict.set_item("stderr", outcome.stderr.clone())?;
    Ok(dict)
}

pub(crate) fn parse_outcome(body: &serde_json::Value) -> OperationOutcome {
    OperationOutcome {
        success: body
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        message: body
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        exit_code: body
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .and_then(|n| {
                if n >= i32::MIN as i64 && n <= i32::MAX as i64 {
                    Some(n as i32)
                } else {
                    None
                }
            }),
        stdout: body
            .get("stdout")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        stderr: body
            .get("stderr")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }
}

pub(crate) fn send_op(url: &str, req: &OpRequest, timeout: f64) -> OperationOutcome {
    // Sync wrapper around `send_op_async` (FastLED/fbuild#817): builds a
    // current-thread tokio runtime per call so we don't duplicate the HTTP
    // transport code. If the runtime fails to build we surface a structured
    // failure outcome identical to a network error.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let msg = format!("failed to build tokio runtime: {}", e);
            eprintln!("[fbuild] {}", msg);
            return OperationOutcome {
                success: false,
                message: Some(msg),
                ..Default::default()
            };
        }
    };
    rt.block_on(send_op_async(url.to_string(), req.clone(), timeout))
}

/// Native-async counterpart to `send_op`. Issues the same HTTP POST against
/// the daemon but yields on I/O instead of blocking a thread, so callers on
/// an asyncio event loop don't need FastLED's `_run_in_thread` shim.
///
/// Returns the same `OperationOutcome` so the sync and async surfaces share
/// `parse_outcome` and `outcome_to_pydict`. See FastLED/fbuild#65.
pub(crate) async fn send_op_async(url: String, req: OpRequest, timeout: f64) -> OperationOutcome {
    let client = fbuild_core::http::client();
    let request = client
        .post(&url)
        .json(&req)
        .timeout(std::time::Duration::from_secs_f64(timeout));

    match request.send().await {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(body) => {
                let outcome = parse_outcome(&body);
                if !outcome.success {
                    if let Some(ref msg) = outcome.message {
                        eprintln!("[fbuild] operation failed: {}", msg);
                    }
                    if let Some(ref stderr) = outcome.stderr {
                        if !stderr.is_empty() {
                            eprintln!("[fbuild] stderr:\n{}", stderr);
                        }
                    }
                }
                outcome
            }
            Err(e) => {
                let msg = format!("failed to parse daemon response: {}", e);
                eprintln!("[fbuild] {}", msg);
                OperationOutcome {
                    success: false,
                    message: Some(msg),
                    ..Default::default()
                }
            }
        },
        Err(e) => {
            let msg = format!("request failed: {}", e);
            eprintln!("[fbuild] {}", msg);
            OperationOutcome {
                success: false,
                message: Some(msg),
                ..Default::default()
            }
        }
    }
}
