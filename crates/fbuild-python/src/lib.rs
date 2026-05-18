//! PyO3 Python bindings for fbuild.
//!
//! Exposes the Rust implementation as a Python module that is API-compatible
//! with the original Python fbuild package. FastLED and other consumers
//! can `from fbuild.api import SerialMonitor` and get the Rust implementation.
//!
//! ## Exposed Python API
//!
//! ```python
//! # Direct import (backwards compatible)
//! from fbuild import Daemon, BuildContext, connect_daemon, __version__
//! from fbuild.api import SerialMonitor, AsyncSerialMonitor
//! from fbuild.daemon import ensure_daemon_running, stop_daemon, is_daemon_running
//! ```
//!
//! ## Architecture
//!
//! Python classes are thin wrappers around Rust types. The SerialMonitor
//! maintains a tokio runtime internally for async serial operations,
//! exposed as sync methods via `block_on()`.
//!
//! Implementation is split across topic-focused submodules; this file is
//! intentionally slim and contains only the `#[pymodule]` entry point,
//! the version constant, the small standalone `#[pyfunction]`s, and the
//! integration tests. All Python-visible classes and helper types live in
//! their respective sibling modules — see `mod` declarations below.

#![allow(clippy::useless_conversion)]

use pyo3::prelude::*;

mod async_daemon_connection;
mod async_serial_monitor;
mod daemon;
mod daemon_connection;
mod json_rpc;
mod messages;
mod outcome;
mod serial_monitor;

use async_daemon_connection::AsyncDaemonConnection;
use async_serial_monitor::AsyncSerialMonitor;
use daemon::{AsyncDaemon, Daemon};
use daemon_connection::DaemonConnection;
use serial_monitor::SerialMonitor;

/// Factory function matching `from fbuild import connect_daemon`.
#[pyfunction]
fn connect_daemon(project_dir: String, environment: String) -> DaemonConnection {
    DaemonConnection::new(project_dir, environment)
}

/// Async-flavored factory matching `connect_daemon` but returning the native
/// async counterpart. Convenience for callers already under `asyncio.run`.
#[pyfunction]
fn connect_daemon_async(project_dir: String, environment: String) -> AsyncDaemonConnection {
    AsyncDaemonConnection::new(project_dir, environment)
}

/// The version string exposed to Python as `fbuild.__version__`.
///
/// Sourced from `CARGO_PKG_VERSION` at compile time so it always tracks the
/// workspace version declared in the root `Cargo.toml`. Do not hardcode this
/// string — a stale literal (previously `"2.0.0"`) made freshness checks
/// against the native binary unreliable.
const PYTHON_MODULE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The fbuild Python module (imported as fbuild._native).
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", PYTHON_MODULE_VERSION)?;
    m.add_class::<SerialMonitor>()?;
    m.add_class::<AsyncSerialMonitor>()?;
    m.add_class::<Daemon>()?;
    m.add_class::<AsyncDaemon>()?;
    m.add_class::<DaemonConnection>()?;
    m.add_class::<AsyncDaemonConnection>()?;
    m.add_function(wrap_pyfunction!(connect_daemon, m)?)?;
    m.add_function(wrap_pyfunction!(connect_daemon_async, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::json_rpc::{extract_remote_json_rpc_response, wait_for_remote_json_rpc_response};
    use crate::outcome::{parse_outcome, send_op_async, OpRequest};
    use crate::PYTHON_MODULE_VERSION;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// `parse_outcome` must faithfully extract every field the daemon's
    /// `OperationResponse` populates so Python callers can branch on the
    /// specific failure mode (see FastLED/fbuild#18). If any field is
    /// silently dropped, the structured-result API offers no more
    /// information than the legacy bool.
    #[test]
    fn parse_outcome_extracts_all_fields() {
        let body = serde_json::json!({
            "success": false,
            "message": "build failed",
            "exit_code": 2,
            "stdout": "compile log",
            "stderr": "error: missing header",
        });
        let outcome = parse_outcome(&body);
        assert!(!outcome.success);
        assert_eq!(outcome.message.as_deref(), Some("build failed"));
        assert_eq!(outcome.exit_code, Some(2));
        assert_eq!(outcome.stdout.as_deref(), Some("compile log"));
        assert_eq!(outcome.stderr.as_deref(), Some("error: missing header"));
    }

    /// The daemon omits `stdout`, `stderr`, and `exit_code` on many success
    /// responses. `parse_outcome` must treat missing fields as `None`
    /// rather than defaulting to empty strings or zero, so Python callers
    /// can distinguish "no data" from "empty data".
    #[test]
    fn parse_outcome_treats_missing_fields_as_none() {
        let body = serde_json::json!({
            "success": true,
            "message": "done",
        });
        let outcome = parse_outcome(&body);
        assert!(outcome.success);
        assert_eq!(outcome.message.as_deref(), Some("done"));
        assert_eq!(outcome.exit_code, None);
        assert_eq!(outcome.stdout, None);
        assert_eq!(outcome.stderr, None);
    }

    /// A malformed or empty response body must not panic and must default
    /// to a failure outcome so callers don't mistakenly treat a garbage
    /// response as success.
    #[test]
    fn parse_outcome_defaults_to_failure_on_empty_body() {
        let outcome = parse_outcome(&serde_json::json!({}));
        assert!(!outcome.success);
        assert_eq!(outcome.message, None);
    }

    /// Ensures the Python-visible `__version__` is sourced from Cargo and not
    /// a stale hardcoded literal. The previous value `"2.0.0"` diverged from
    /// the workspace version and broke native-binary freshness checks.
    #[test]
    fn python_module_version_matches_pkg_version() {
        assert_eq!(PYTHON_MODULE_VERSION, env!("CARGO_PKG_VERSION"));
        assert_ne!(
            PYTHON_MODULE_VERSION, "2.0.0",
            "fbuild-python __version__ must not be hardcoded to the legacy 2.0.0 literal"
        );
    }

    /// Guards against malformed version strings leaking into the Python
    /// module. Accepts `MAJOR.MINOR.PATCH` with optional pre-release/build
    /// metadata (e.g. `2.1.5`, `2.1.5-rc1`, `2.1.5+build.7`).
    #[test]
    fn python_module_version_is_valid_semver_shape() {
        let version = PYTHON_MODULE_VERSION;
        assert!(!version.is_empty(), "version must not be empty");

        // Strip optional pre-release (-xxx) and build metadata (+xxx) suffixes
        // before splitting on '.'.
        let core = version
            .split_once('-')
            .map(|(c, _)| c)
            .unwrap_or(version)
            .split_once('+')
            .map(|(c, _)| c)
            .unwrap_or_else(|| version.split_once('-').map(|(c, _)| c).unwrap_or(version));

        let parts: Vec<&str> = core.split('.').collect();
        assert_eq!(
            parts.len(),
            3,
            "version {version:?} must have MAJOR.MINOR.PATCH components"
        );
        for (name, part) in ["major", "minor", "patch"].iter().zip(parts.iter()) {
            assert!(
                part.parse::<u64>().is_ok(),
                "version {name} component {part:?} must be a non-negative integer"
            );
        }
    }

    #[test]
    fn extract_remote_json_rpc_response_skips_empty_batches() {
        let empty: Vec<String> = vec![];
        assert_eq!(extract_remote_json_rpc_response(&empty), None);
    }

    #[test]
    fn extract_remote_json_rpc_response_finds_remote_payload() {
        let lines = vec![
            "noise".to_string(),
            r#"REMOTE: {"ok": true}"#.to_string(),
            "more noise".to_string(),
        ];
        assert_eq!(
            extract_remote_json_rpc_response(&lines).as_deref(),
            Some(r#" {"ok": true}"#)
        );
    }

    #[test]
    fn wait_for_remote_json_rpc_response_keeps_polling_after_empty_batch() {
        let mut polls = 0usize;
        let result = wait_for_remote_json_rpc_response(0.05, |_| {
            polls += 1;
            match polls {
                1 => vec![],
                2 => vec!["REMOTE: {\"ok\": true}".to_string()],
                _ => vec![],
            }
        });

        assert_eq!(polls, 2, "an empty batch must not end the overall wait");
        assert_eq!(result.as_deref(), Some(r#" {"ok": true}"#));
    }

    fn sample_op_request() -> OpRequest {
        OpRequest {
            project_dir: "tests/platform/uno".into(),
            environment: Some("uno".into()),
            clean_build: false,
            verbose: false,
            port: None,
            monitor_after: false,
            skip_build: false,
            baud_rate: None,
        }
    }

    /// Minimal in-process HTTP mock. Accepts a single connection, reads
    /// the request (ignored), replies with `body` as a JSON 200 OK, and
    /// returns the bound address for the caller to point reqwest at.
    ///
    /// Deliberately does not pull a crate dep — axum is already in the
    /// workspace but not in `fbuild-python`'s dep graph, and adding it
    /// just for one test would inflate the build graph for every clean
    /// `uv run soldr cargo check`. Raw TCP is adequate for a response we control.
    async fn spawn_mock_daemon(body: String) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                // Drain the request so reqwest sees the response arrive.
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{}/api/build", addr)
    }

    /// `send_op_async` must parse a successful response identically to
    /// the blocking `send_op`, so the AsyncDaemonConnection surface
    /// returns the same OperationOutcome fields as the sync sibling.
    #[test]
    fn send_op_async_parses_success_response() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let url = spawn_mock_daemon(
                r#"{"success":true,"message":"ok","exit_code":0,"stdout":"","stderr":""}"#.into(),
            )
            .await;
            let outcome = send_op_async(url, sample_op_request(), 5.0).await;
            assert!(outcome.success, "expected success=true from mock");
            assert_eq!(outcome.message.as_deref(), Some("ok"));
            assert_eq!(outcome.exit_code, Some(0));
        });
    }

    /// `send_op_async` must surface structured failure fields (message,
    /// exit_code, stderr) exactly like `send_op`, so callers porting to
    /// async don't regress in what they can branch on.
    #[test]
    fn send_op_async_parses_failure_response() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let url = spawn_mock_daemon(
                r#"{"success":false,"message":"build failed","exit_code":2,"stderr":"compile error"}"#
                    .into(),
            )
            .await;
            let outcome = send_op_async(url, sample_op_request(), 5.0).await;
            assert!(!outcome.success);
            assert_eq!(outcome.message.as_deref(), Some("build failed"));
            assert_eq!(outcome.exit_code, Some(2));
            assert_eq!(outcome.stderr.as_deref(), Some("compile error"));
        });
    }

    /// Connection errors must materialize as `success=false` with a
    /// descriptive message, matching the sync contract. This guards
    /// against the async path panicking when the daemon is not up.
    #[test]
    fn send_op_async_returns_failure_outcome_on_connection_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Unroutable address (reserved TEST-NET-1). reqwest will fail
            // fast with a connect error instead of hanging to the timeout.
            let url = "http://192.0.2.1:1/api/build".to_string();
            let outcome = send_op_async(url, sample_op_request(), 1.0).await;
            assert!(!outcome.success);
            assert!(
                outcome
                    .message
                    .as_deref()
                    .map(|m| m.contains("request failed"))
                    .unwrap_or(false),
                "expected 'request failed' message, got {:?}",
                outcome.message
            );
        });
    }
}
