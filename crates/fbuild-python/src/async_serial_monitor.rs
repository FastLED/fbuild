//! Asynchronous `AsyncSerialMonitor` PyO3 binding — the native async
//! counterpart to `SerialMonitor` (FastLED/fbuild#65, promoted out of
//! experimental by FastLED/fbuild#1485 §4.7).
//!
//! A thin facade over the shared [`crate::serial_session::SerialSession`]
//! core: every awaitable comes from `pyo3_async_runtimes::tokio::future_into_py`
//! over an `Arc<tokio::sync::RwLock<Option<Arc<SerialSession>>>>`.
//!
//! The inner `Arc<SerialSession>` (not a bare `SerialSession`) matters:
//! every operation takes the **read** lock only long enough to clone that
//! `Arc`, then releases it before actually awaiting the (possibly
//! long-running) operation. If operations held the read lock for their
//! whole duration instead, `__aexit__`'s write-lock acquisition — needed
//! to `take()` the session out — would have to wait for every in-flight
//! reader to finish first, defeating the point of AT-P15 ("`__aexit__`
//! while calls are in flight" must unblock those calls promptly, not the
//! other way around). With the `Arc<SerialSession>` clone released
//! immediately, `__aexit__` can grab the write lock right away, call
//! `SerialSession::close()` (which takes `&self`, see `serial_session.rs`),
//! and every in-flight call — still holding its own `Arc` clone — observes
//! the session transition to `Closed` and returns/raises promptly.
//! A separate async lifecycle mutex serializes enter, exit, and reset, so
//! one lifecycle operation cannot replace a session created by another.

use pyo3::prelude::*;
use serde::Serialize;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;

use crate::serial_session::{SerialSession, SessionConfig, SessionError, SessionStatus};

fn map_err(err: SessionError) -> PyErr {
    match err {
        SessionError::Timeout => pyo3::exceptions::PyTimeoutError::new_err(err.to_string()),
        SessionError::Closed
        | SessionError::ConnectionFailed(_)
        | SessionError::PortGone
        | SessionError::Preempted => pyo3::exceptions::PyConnectionError::new_err(err.to_string()),
        SessionError::ProtocolDesync | SessionError::WriteFailed(_) => {
            pyo3::exceptions::PyRuntimeError::new_err(err.to_string())
        }
    }
}

fn not_open_err() -> PyErr {
    pyo3::exceptions::PyConnectionError::new_err("AsyncSerialMonitor session is not open")
}

/// The async bridge can discard a completed Rust result if Python cancels
/// before `Future.set_result` runs on the event loop. Keep a copy of the
/// drained batch until Python's Future completes; cancellation restores it.
#[derive(Default)]
struct PendingReadDelivery {
    cancelled: AtomicBool,
    batch: StdMutex<Option<(Arc<SerialSession>, Vec<String>)>>,
}

#[pyclass]
struct ReadDoneCallback {
    pending: Arc<PendingReadDelivery>,
}

#[pymethods]
impl ReadDoneCallback {
    fn __call__(&self, future: &Bound<'_, PyAny>) -> PyResult<()> {
        let cancelled: bool = future.call_method0("cancelled")?.extract()?;
        if cancelled {
            self.pending.cancelled.store(true, Ordering::SeqCst);
        }
        let batch = self
            .pending
            .batch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if cancelled {
            if let Some((session, lines)) = batch {
                session.restore_cancelled_read(lines);
            }
        }
        Ok(())
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
    session: Arc<RwLock<Option<Arc<SerialSession>>>>,
    lifecycle: Arc<tokio::sync::Mutex<()>>,
}

impl AsyncSerialMonitor {
    fn config(&self) -> SessionConfig {
        SessionConfig {
            ws_url: format!(
                "ws://127.0.0.1:{}/ws/serial-monitor",
                fbuild_paths::get_daemon_port()
            ),
            port: self.port.clone(),
            baud_rate: self.baud_rate,
            auto_reconnect: self.auto_reconnect,
            verbose: self.verbose,
            client_id: self.client_id.clone(),
            max_buffered_lines: crate::serial_session::DEFAULT_MAX_BUFFERED_LINES,
            handshake_timeout: std::time::Duration::from_secs(5),
        }
    }
}

/// Clone the current session `Arc` (if any) without holding the lock any
/// longer than that.
async fn current_session(
    slot: &Arc<RwLock<Option<Arc<SerialSession>>>>,
) -> Option<Arc<SerialSession>> {
    slot.read().await.clone()
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
            lifecycle: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Number of oldest lines discarded because the bounded queue filled.
    #[getter]
    fn lines_dropped(&self) -> usize {
        self.session
            .try_read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|session| session.lines_dropped()))
            .unwrap_or(0)
    }

    fn __aenter__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let cfg = slf.config();
        let session_slot = slf.session.clone();
        let lifecycle = slf.lifecycle.clone();
        let slf_obj = slf.into_pyobject(py)?.unbind().into_any();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let _lifecycle = lifecycle.lock().await;
            let session = SerialSession::connect(cfg).await.map_err(map_err)?;
            let old = session_slot.write().await.replace(Arc::new(session));
            if let Some(old) = old {
                old.close().await;
            }
            Ok(slf_obj)
        })
    }

    /// `__aexit__` while calls are in flight in other tasks: those
    /// awaitables observe the session close and raise `ConnectionError`
    /// (§AT-P15), not hang. See the module docs for why this doesn't wait
    /// for in-flight readers.
    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: Option<Py<PyAny>>,
        _exc_val: Option<Py<PyAny>>,
        _exc_tb: Option<Py<PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let session_slot = self.session.clone();
        let lifecycle = self.lifecycle.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let _lifecycle = lifecycle.lock().await;
            let session = session_slot.write().await.take();
            if let Some(session) = session {
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
        if timeout_secs.is_some() {
            PyErr::warn(
                py,
                &py.get_type::<pyo3::exceptions::PyDeprecationWarning>(),
                c"timeout_secs is deprecated; use timeout instead",
                1,
            )?;
        }
        let timeout = timeout_secs.unwrap_or(timeout);
        let session_slot = self.session.clone();
        let pending = Arc::new(PendingReadDelivery::default());
        let callback = Py::new(
            py,
            ReadDoneCallback {
                pending: Arc::clone(&pending),
            },
        )?;
        let future = pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let Some(session) = current_session(&session_slot).await else {
                return Err(not_open_err());
            };
            let lines = session
                .read_lines(std::time::Duration::from_secs_f64(timeout.max(0.0)))
                .await;
            if *session.status().borrow() == SessionStatus::Closed {
                return Err(map_err(SessionError::Closed));
            }
            let mut pending_batch = pending.batch.lock().unwrap_or_else(|e| e.into_inner());
            if pending.cancelled.load(Ordering::SeqCst) {
                drop(pending_batch);
                session.restore_cancelled_read(lines);
                return Ok(Vec::new());
            }
            *pending_batch = Some((session, lines.clone()));
            Ok(lines)
        })?;
        future.call_method1("add_done_callback", (callback,))?;
        Ok(future)
    }

    /// Returns the number of bytes written (**breaking change** from the
    /// pre-#1485 `bool`; see the release notes and `docs/architecture/pyo3-bindings.md`).
    fn write<'py>(&self, py: Python<'py>, data: &str) -> PyResult<Bound<'py, PyAny>> {
        let session_slot = self.session.clone();
        let data = data.to_string();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let Some(session) = current_session(&session_slot).await else {
                return Err(not_open_err());
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
            let Some(session) = current_session(&session_slot).await else {
                return Err(not_open_err());
            };
            let reply = session
                .json_rpc(
                    &json_str,
                    std::time::Duration::from_secs_f64(timeout.max(0.0)),
                )
                .await
                .map_err(map_err)?;
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
            let Some(session) = current_session(&session_slot).await else {
                return Err(not_open_err());
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
            if let Some(session) = current_session(&session_slot).await {
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
        let session_slot = self.session.clone();
        let lifecycle = self.lifecycle.clone();
        let cfg = self.config();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let _lifecycle = lifecycle.lock().await;
            let old = session_slot.write().await.take();
            let was_connected = old.is_some();
            if let Some(session) = old {
                session.close().await;
            }
            let success = post_reset_request_async(port, board).await?;
            if was_connected && success {
                let new_session = SerialSession::connect(cfg).await.map_err(map_err)?;
                *session_slot.write().await = Some(Arc::new(new_session));
            }

            if !success || !wait_for_output {
                return Ok(success);
            }

            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let deadline =
                tokio::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
            while tokio::time::Instant::now() < deadline {
                let remaining = (deadline - tokio::time::Instant::now())
                    .min(std::time::Duration::from_millis(200));
                let Some(session) = current_session(&session_slot).await else {
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
