//! Asynchronous `AsyncSerialMonitor` PyO3 binding — the native async
//! counterpart to `SerialMonitor` (FastLED/fbuild#65, promoted out of
//! experimental by FastLED/fbuild#1485 §4.7).
//!
//! A thin facade over the shared [`crate::serial_session::SerialSession`]
//! core: every awaitable comes from `pyo3_async_runtimes::tokio::future_into_py`
//! over an `Arc<tokio::sync::RwLock<Option<SerialSession>>>`, so cancelling
//! the asyncio task cancels the underlying Rust future without corrupting
//! the shared core (cancel-safety lives in `SerialSession` itself).

use pyo3::prelude::*;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::serial_session::{SerialSession, SessionConfig, SessionError};

fn map_err(err: SessionError) -> PyErr {
    match err {
        SessionError::Timeout => pyo3::exceptions::PyTimeoutError::new_err(err.to_string()),
        SessionError::Closed | SessionError::PortGone | SessionError::Preempted => {
            pyo3::exceptions::PyConnectionError::new_err(err.to_string())
        }
        SessionError::ProtocolDesync => pyo3::exceptions::PyRuntimeError::new_err(err.to_string()),
    }
}

/// Python-visible AsyncSerialMonitor class: a supported API (see §4.7 of
/// FastLED/fbuild#1485), not experimental.
///
/// ```python
/// import asyncio
/// from fbuild._native import AsyncSerialMonitor
///
/// async def main():
///     async with AsyncSerialMonitor(port="COM13", baud_rate=115200) as mon:
///         lines = await mon.read_lines(timeout=5.0)
///         n = await mon.write("hello\n")
///         ok = await mon.reset_device(board="esp32s3")
///
/// asyncio.run(main())
/// ```
#[pyclass]
pub(crate) struct AsyncSerialMonitor {
    port: String,
    baud_rate: u32,
    auto_reconnect: bool,
    verbose: bool,
    client_id: String,
    session: Arc<RwLock<Option<SerialSession>>>,
}

#[pymethods]
impl AsyncSerialMonitor {
    #[new]
    #[pyo3(signature = (port, baud_rate=115200, auto_reconnect=true, verbose=false))]
    fn new(port: String, baud_rate: u32, auto_reconnect: bool, verbose: bool) -> Self {
        Self {
            port,
            baud_rate,
            auto_reconnect,
            verbose,
            client_id: uuid::Uuid::new_v4().to_string(),
            session: Arc::new(RwLock::new(None)),
        }
    }

    fn __aenter__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let cfg = SessionConfig {
            ws_url: format!(
                "ws://127.0.0.1:{}/ws/serial-monitor",
                fbuild_paths::get_daemon_port()
            ),
            port: slf.port.clone(),
            baud_rate: slf.baud_rate,
            auto_reconnect: slf.auto_reconnect,
            verbose: slf.verbose,
            client_id: slf.client_id.clone(),
            max_buffered_lines: crate::serial_session::DEFAULT_MAX_BUFFERED_LINES,
            handshake_timeout: std::time::Duration::from_secs(5),
        };
        let session_slot = slf.session.clone();
        let slf_obj = slf.into_pyobject(py)?.unbind().into_any();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let session = SerialSession::connect(cfg).await.map_err(map_err)?;
            *session_slot.write().await = Some(session);
            Ok(slf_obj)
        })
    }

    /// `__aexit__` while calls are in flight in other tasks: those
    /// awaitables observe the session close and raise `ConnectionError`
    /// (§AT-P15), not hang.
    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: Option<Py<PyAny>>,
        _exc_val: Option<Py<PyAny>>,
        _exc_tb: Option<Py<PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let session_slot = self.session.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            if let Some(session) = session_slot.write().await.take() {
                session.close().await;
            }
            Ok(false)
        })
    }

    /// `timeout_secs=` is kept as a deprecated alias for `timeout=` for one
    /// release (§4.7).
    #[pyo3(signature = (timeout=30.0, timeout_secs=None))]
    fn read_lines<'py>(
        &self,
        py: Python<'py>,
        timeout: f64,
        timeout_secs: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let timeout = timeout_secs.unwrap_or(timeout);
        let session_slot = self.session.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = session_slot.read().await;
            let Some(session) = guard.as_ref() else {
                return Err(pyo3::exceptions::PyConnectionError::new_err(
                    "AsyncSerialMonitor session is not open",
                ));
            };
            Ok(session
                .read_lines(std::time::Duration::from_secs_f64(timeout.max(0.0)))
                .await)
        })
    }

    /// Returns the number of bytes written (**breaking change** from the
    /// pre-#1485 `bool`; see the release notes and `docs/architecture/pyo3-bindings.md`).
    fn write<'py>(&self, py: Python<'py>, data: &str) -> PyResult<Bound<'py, PyAny>> {
        let session_slot = self.session.clone();
        let data = data.to_string();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = session_slot.read().await;
            let Some(session) = guard.as_ref() else {
                return Err(pyo3::exceptions::PyConnectionError::new_err(
                    "AsyncSerialMonitor session is not open",
                ));
            };
            session
                .write(data.as_bytes(), std::time::Duration::from_secs(5))
                .await
                .map_err(map_err)
        })
    }

    #[pyo3(signature = (request, timeout=5.0))]
    fn write_json_rpc<'py>(
        &self,
        py: Python<'py>,
        request: &Bound<'_, PyAny>,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let json_str: String = py
            .import("json")?
            .call_method1("dumps", (request,))?
            .extract()?;
        let session_slot = self.session.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let reply = {
                let guard = session_slot.read().await;
                let Some(session) = guard.as_ref() else {
                    return Err(pyo3::exceptions::PyConnectionError::new_err(
                        "AsyncSerialMonitor session is not open",
                    ));
                };
                session
                    .json_rpc(
                        &json_str,
                        std::time::Duration::from_secs_f64(timeout.max(0.0)),
                    )
                    .await
                    .map_err(map_err)?
            };
            Python::attach(|py| {
                let json_module = py.import("json")?;
                let parsed = json_module.call_method1("loads", (reply.trim(),))?;
                Ok(parsed.unbind())
            })
        })
    }

    /// Awaitable method (a property can't be awaited, §4.7).
    fn in_waiting<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let session_slot = self.session.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = session_slot.read().await;
            let Some(session) = guard.as_ref() else {
                return Err(pyo3::exceptions::PyConnectionError::new_err(
                    "AsyncSerialMonitor session is not open",
                ));
            };
            session
                .in_waiting(std::time::Duration::from_secs(2))
                .await
                .map_err(map_err)
        })
    }

    fn reset_input_buffer<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let session_slot = self.session.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            if let Some(session) = session_slot.read().await.as_ref() {
                session.clear_input().await;
            }
            Ok(())
        })
    }

    /// Asynchronously reset the device via the daemon's `POST /api/reset`
    /// endpoint, then (optionally) wait for post-reset output.
    #[pyo3(signature = (board=None, wait_for_output=false, timeout=5.0))]
    fn reset_device<'py>(
        &self,
        py: Python<'py>,
        board: Option<String>,
        wait_for_output: bool,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let port = self.port.clone();
        let auto_reconnect = self.auto_reconnect;
        let session_slot = self.session.clone();
        let cfg_port = self.port.clone();
        let baud_rate = self.baud_rate;
        let verbose = self.verbose;
        let client_id = self.client_id.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let success = post_reset_request_async(port, board).await?;

            let mut guard = session_slot.write().await;
            let was_connected = guard.is_some();
            if was_connected {
                if let Some(session) = guard.take() {
                    session.close().await;
                }
                if success && auto_reconnect {
                    let cfg = SessionConfig {
                        ws_url: format!(
                            "ws://127.0.0.1:{}/ws/serial-monitor",
                            fbuild_paths::get_daemon_port()
                        ),
                        port: cfg_port,
                        baud_rate,
                        auto_reconnect,
                        verbose,
                        client_id,
                        max_buffered_lines: crate::serial_session::DEFAULT_MAX_BUFFERED_LINES,
                        handshake_timeout: std::time::Duration::from_secs(5),
                    };
                    *guard = SerialSession::connect(cfg).await.ok();
                }
            }
            drop(guard);

            if !success || !wait_for_output {
                return Ok(success);
            }

            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let deadline =
                tokio::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
            while tokio::time::Instant::now() < deadline {
                let remaining = (deadline - tokio::time::Instant::now())
                    .min(std::time::Duration::from_millis(200));
                let guard = session_slot.read().await;
                let Some(session) = guard.as_ref() else {
                    break;
                };
                let lines = session.read_lines(remaining).await;
                if !lines.is_empty() {
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }
}

/// Issue the daemon's `POST /api/reset` and return whether the daemon
/// reported success. Shared between `AsyncSerialMonitor::reset_device` and
/// the sync `SerialMonitor::reset_device` (FastLED/fbuild#817).
pub(crate) async fn post_reset_request_async(
    port: String,
    board: Option<String>,
) -> PyResult<bool> {
    #[derive(Serialize)]
    struct ResetPayload {
        port: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        board: Option<String>,
    }

    let url = format!("{}/api/reset", fbuild_paths::get_daemon_url());
    let payload = ResetPayload { port, board };

    let resp = fbuild_core::http::client()
        .post(&url)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            pyo3::exceptions::PyConnectionError::new_err(format!(
                "failed to send reset request to daemon: {e}"
            ))
        })?;

    let body: serde_json::Value = resp.json().await.map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("failed to parse reset response: {e}"))
    })?;

    Ok(body
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}
