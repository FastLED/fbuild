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
    use crate::PYTHON_MODULE_VERSION;
    use crate::json_rpc::{extract_remote_json_rpc_response, wait_for_remote_json_rpc_response};
    use crate::outcome::{OpRequest, parse_outcome, platformio_src_dir_from_env, send_op_async};
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serializes tests that mutate `PLATFORMIO_SRC_DIR`.
    ///
    /// `std::env::set_var` and `remove_var` mutate process-global state, so
    /// running env-var tests in parallel (cargo's default) creates races
    /// where one test sees another's value and the assertions flake. A
    /// single `Mutex` held across set → call → assert → restore keeps the
    /// env-mutating tests strictly serial without forcing the whole crate
    /// onto `--test-threads=1`.
    static PLATFORMIO_SRC_DIR_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard that restores `PLATFORMIO_SRC_DIR` on drop.
    ///
    /// Holds the env-var lock for its lifetime so concurrent env-var tests
    /// queue rather than race. The previous value is restored exactly as
    /// observed (including "unset") so tests don't leak state into siblings
    /// that run after them.
    struct PlatformioSrcDirGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    impl PlatformioSrcDirGuard {
        fn acquire() -> Self {
            // PoisonError is fine: the guard exists purely to serialize
            // env-var access, and a poisoned mutex still serializes.
            let lock = PLATFORMIO_SRC_DIR_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var("PLATFORMIO_SRC_DIR").ok();
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for PlatformioSrcDirGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => std::env::set_var("PLATFORMIO_SRC_DIR", v),
                None => std::env::remove_var("PLATFORMIO_SRC_DIR"),
            }
        }
    }

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
            src_dir: None,
        }
    }

    /// Minimal in-process HTTP mock. Accepts a single connection, reads
    /// the request (ignored), replies with `body` as a JSON 200 OK, and
    /// returns the bound address for the caller to point reqwest at.
    ///
    /// Deliberately does not pull a crate dep — axum is already in the
    /// workspace but not in `fbuild-python`'s dep graph, and adding it
    /// just for one test would inflate the build graph for every clean
    /// `soldr cargo check`. Raw TCP is adequate for a response we control.
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

    /// When `PLATFORMIO_SRC_DIR` is set, the helper must return its value
    /// verbatim. This is the env-read primitive both DaemonConnection
    /// surfaces use to populate `OpRequest.src_dir`, so FastLED's
    /// autoresearch override survives the Python -> daemon hop. See
    /// FastLED/fbuild#274.
    #[test]
    fn platformio_src_dir_helper_returns_value_when_set() {
        let _guard = PlatformioSrcDirGuard::acquire();
        std::env::set_var("PLATFORMIO_SRC_DIR", "examples/AutoResearch");
        assert_eq!(
            platformio_src_dir_from_env().as_deref(),
            Some("examples/AutoResearch")
        );
    }

    /// When `PLATFORMIO_SRC_DIR` is unset, the helper must return `None`
    /// so `OpRequest.src_dir` stays `None` and the daemon falls back to
    /// `platformio.ini`'s configured `src_dir`. Mirrors the CLI's
    /// `.ok().filter(|s| !s.is_empty())` contract.
    #[test]
    fn platformio_src_dir_helper_returns_none_when_unset() {
        let _guard = PlatformioSrcDirGuard::acquire();
        std::env::remove_var("PLATFORMIO_SRC_DIR");
        assert_eq!(platformio_src_dir_from_env(), None);
    }

    /// An empty `PLATFORMIO_SRC_DIR` (`""`) must be treated as unset, not
    /// forwarded as an empty string. The CLI uses the same `filter(|s|
    /// !s.is_empty())` rule and a stray empty value would tell the daemon
    /// to compile an empty directory.
    #[test]
    fn platformio_src_dir_helper_returns_none_when_empty() {
        let _guard = PlatformioSrcDirGuard::acquire();
        std::env::set_var("PLATFORMIO_SRC_DIR", "");
        assert_eq!(platformio_src_dir_from_env(), None);
    }

    /// `DaemonConnection::build_request` must forward `PLATFORMIO_SRC_DIR`
    /// into `OpRequest.src_dir` so the daemon receives the override the
    /// caller set on the parent env, matching `fbuild-cli`'s `Build`
    /// request construction. Regression guard for FastLED/fbuild#274.
    #[test]
    fn daemon_connection_build_request_forwards_platformio_src_dir() {
        let _guard = PlatformioSrcDirGuard::acquire();
        std::env::set_var("PLATFORMIO_SRC_DIR", "examples/AutoResearch");
        let conn = crate::daemon_connection::DaemonConnection::new(
            "tests/platform/uno".into(),
            "uno".into(),
        );
        let req = conn.build_request(false, false);
        assert_eq!(req.src_dir.as_deref(), Some("examples/AutoResearch"));
    }

    /// `DaemonConnection::deploy_request` must forward `PLATFORMIO_SRC_DIR`
    /// for the same reason `build_request` does — the issue's "Done"
    /// criteria explicitly call out deploy parity with the CLI.
    #[test]
    fn daemon_connection_deploy_request_forwards_platformio_src_dir() {
        let _guard = PlatformioSrcDirGuard::acquire();
        std::env::set_var("PLATFORMIO_SRC_DIR", "examples/AutoResearch");
        let conn = crate::daemon_connection::DaemonConnection::new(
            "tests/platform/uno".into(),
            "uno".into(),
        );
        let req = conn.deploy_request(None, false, false, false);
        assert_eq!(req.src_dir.as_deref(), Some("examples/AutoResearch"));
    }

    /// When the env var is unset, `build_request` must leave `src_dir` as
    /// `None`. Omitting the field on the wire is what lets the daemon fall
    /// back to `platformio.ini`'s `src_dir`; a forwarded `Some("")` would
    /// break that fallback.
    #[test]
    fn daemon_connection_build_request_omits_src_dir_when_env_unset() {
        let _guard = PlatformioSrcDirGuard::acquire();
        std::env::remove_var("PLATFORMIO_SRC_DIR");
        let conn = crate::daemon_connection::DaemonConnection::new(
            "tests/platform/uno".into(),
            "uno".into(),
        );
        let req = conn.build_request(false, false);
        assert!(req.src_dir.is_none());
    }

    /// Same omission guarantee for deploy. The CLI and Python paths must
    /// behave identically when the caller has not set
    /// `PLATFORMIO_SRC_DIR`.
    #[test]
    fn daemon_connection_deploy_request_omits_src_dir_when_env_unset() {
        let _guard = PlatformioSrcDirGuard::acquire();
        std::env::remove_var("PLATFORMIO_SRC_DIR");
        let conn = crate::daemon_connection::DaemonConnection::new(
            "tests/platform/uno".into(),
            "uno".into(),
        );
        let req = conn.deploy_request(None, false, false, false);
        assert!(req.src_dir.is_none());
    }

    /// Async parity with the sync `build_request` forwarding check. The
    /// AsyncDaemonConnection is what FastLED uses under asyncio, so a
    /// regression here would surface the same wrong-sketch failure mode
    /// even after the sync path is fixed.
    #[test]
    fn async_daemon_connection_build_request_forwards_platformio_src_dir() {
        let _guard = PlatformioSrcDirGuard::acquire();
        std::env::set_var("PLATFORMIO_SRC_DIR", "examples/AutoResearch");
        let conn = crate::async_daemon_connection::AsyncDaemonConnection::new(
            "tests/platform/uno".into(),
            "uno".into(),
        );
        let req = conn.build_request(false, false);
        assert_eq!(req.src_dir.as_deref(), Some("examples/AutoResearch"));
    }

    /// Async parity with the sync `deploy_request` forwarding check.
    #[test]
    fn async_daemon_connection_deploy_request_forwards_platformio_src_dir() {
        let _guard = PlatformioSrcDirGuard::acquire();
        std::env::set_var("PLATFORMIO_SRC_DIR", "examples/AutoResearch");
        let conn = crate::async_daemon_connection::AsyncDaemonConnection::new(
            "tests/platform/uno".into(),
            "uno".into(),
        );
        let req = conn.deploy_request(None, false, false, false);
        assert_eq!(req.src_dir.as_deref(), Some("examples/AutoResearch"));
    }

    /// Async omission parity: with the env var unset, the async
    /// surface must also leave `src_dir` as `None` so the daemon's
    /// `platformio.ini` fallback still kicks in.
    #[test]
    fn async_daemon_connection_build_request_omits_src_dir_when_env_unset() {
        let _guard = PlatformioSrcDirGuard::acquire();
        std::env::remove_var("PLATFORMIO_SRC_DIR");
        let conn = crate::async_daemon_connection::AsyncDaemonConnection::new(
            "tests/platform/uno".into(),
            "uno".into(),
        );
        let req = conn.build_request(false, false);
        assert!(req.src_dir.is_none());
    }

    /// Async omission parity for deploy.
    #[test]
    fn async_daemon_connection_deploy_request_omits_src_dir_when_env_unset() {
        let _guard = PlatformioSrcDirGuard::acquire();
        std::env::remove_var("PLATFORMIO_SRC_DIR");
        let conn = crate::async_daemon_connection::AsyncDaemonConnection::new(
            "tests/platform/uno".into(),
            "uno".into(),
        );
        let req = conn.deploy_request(None, false, false, false);
        assert!(req.src_dir.is_none());
    }

    /// `OpRequest` serializes `src_dir` with `skip_serializing_if =
    /// "Option::is_none"`, so when the env var is unset the field must not
    /// appear in the JSON sent to the daemon. The daemon's
    /// `BuildRequest.src_dir` is `Option<String>` with `serde(default)`;
    /// omitting the field is the only way to get the platformio.ini
    /// fallback. A forwarded `null` would be equivalent here, but
    /// historical CLI traffic doesn't include the key at all so we keep
    /// parity.
    #[test]
    fn op_request_serializes_src_dir_only_when_set() {
        let mut req = sample_op_request();
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("src_dir"),
            "src_dir must be omitted when None, got {json}"
        );

        req.src_dir = Some("examples/AutoResearch".into());
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            json.contains(r#""src_dir":"examples/AutoResearch""#),
            "src_dir must serialize verbatim when set, got {json}"
        );
    }

    /// `read_lines_async` must drain the cross-call `pending_lines` queue
    /// before touching the wire. The PR fix for write_ack ordering parks
    /// Data frames that arrive ahead of the ack into this queue, so the
    /// next `read_lines` MUST see them even if the wire is idle (or, as
    /// here, never connected).
    #[test]
    fn read_lines_async_drains_pending_before_wire() {
        use crate::json_rpc::read_lines_async;
        use std::collections::VecDeque;
        use std::sync::Arc;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ws_read_slot = Arc::new(tokio::sync::Mutex::new(None));
            let pending = Arc::new(tokio::sync::Mutex::new(VecDeque::from(vec![
                "line-a".to_string(),
                "line-b".to_string(),
            ])));
            let lines = read_lines_async(ws_read_slot, pending.clone(), false, 0.1).await;
            assert_eq!(lines, vec!["line-a".to_string(), "line-b".to_string()]);
            assert!(
                pending.lock().await.is_empty(),
                "pending queue must be drained after read"
            );
        });
    }

    /// Regression guard for the write_ack vs Data ordering bug: if a
    /// `Data` frame lands on the wire before the `WriteAck`, `write_async`
    /// must enqueue the data lines into `pending_lines` (so the next read
    /// surfaces them) AND still return `true` once the ack arrives.
    /// Previous behavior silently consumed the data frame.
    #[test]
    fn write_async_queues_data_arriving_before_ack() {
        use crate::json_rpc::write_async;
        use futures::{SinkExt, StreamExt};
        use std::collections::VecDeque;
        use std::sync::Arc;
        use tokio_tungstenite::tungstenite;

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (sock, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
                let _ = ws.next().await;
                ws.send(tungstenite::Message::Text(
                    r#"{"type":"data","lines":["serial-out"],"current_index":1}"#.to_string(),
                ))
                .await
                .unwrap();
                ws.send(tungstenite::Message::Text(
                    r#"{"type":"write_ack","success":true,"bytes_written":3,"message":null}"#
                        .to_string(),
                ))
                .await
                .unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            });

            let url = format!("ws://{}/", addr);
            let (ws_stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
            let (sink, source) = ws_stream.split();

            let ws_write = Arc::new(tokio::sync::Mutex::new(Some(sink)));
            let ws_read = Arc::new(tokio::sync::Mutex::new(Some(source)));
            let pending = Arc::new(tokio::sync::Mutex::new(VecDeque::<String>::new()));

            let ok = write_async(ws_write, ws_read, pending.clone(), "AAA=".to_string()).await;
            assert!(ok, "write_async must return true after seeing WriteAck");
            let pending_snapshot: Vec<String> = pending.lock().await.iter().cloned().collect();
            assert_eq!(
                pending_snapshot,
                vec!["serial-out".to_string()],
                "data frame arriving before write_ack must be queued for the next read"
            );

            let _ = server.await;
        });
    }

    /// `wait_for_remote_json_rpc_response_async` must consult the
    /// pending queue via `read_lines_async`, so a `REMOTE: ...` line
    /// parked there by `write_async` (Data-before-WriteAck case) is
    /// surfaced on the very next poll — without ever touching the
    /// wire. Closes the loop on the write_ack ordering fix.
    #[test]
    fn wait_for_remote_response_async_picks_up_pending_remote_line() {
        use crate::json_rpc::wait_for_remote_json_rpc_response_async;
        use std::collections::VecDeque;
        use std::sync::Arc;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ws_read_slot = Arc::new(tokio::sync::Mutex::new(None));
            let pending = Arc::new(tokio::sync::Mutex::new(VecDeque::from(vec![
                r#"REMOTE: {"id":1,"result":"ok"}"#.to_string(),
            ])));
            let payload =
                wait_for_remote_json_rpc_response_async(0.5, ws_read_slot, pending, false).await;
            assert_eq!(payload.as_deref(), Some(r#" {"id":1,"result":"ok"}"#));
        });
    }
}
