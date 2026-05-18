//! Asynchronous `AsyncSerialMonitor` PyO3 binding — the native async
//! counterpart to `SerialMonitor` (FastLED/fbuild#65).

use futures::{SinkExt, StreamExt};
use pyo3::prelude::*;
use serde::Serialize;
use std::sync::Arc;
use tokio_tungstenite::tungstenite;

use crate::json_rpc::{
    encode_payload, read_lines_async, wait_for_remote_json_rpc_response_async, write_async,
};
use crate::messages::{ClientMessage, ServerMessage, WsSink, WsSource};

/// Python-visible AsyncSerialMonitor class.
///
/// Native async equivalent of `SerialMonitor`. Exposes every long-running
/// serial operation (`read_lines`, `write`, `write_json_rpc`) plus the
/// context-manager pair (`__aenter__` / `__aexit__`) as `async def` so
/// callers can `await` them directly from an asyncio event loop without
/// FastLED's thread-pool shim (`_run_in_thread`). See FastLED/fbuild#65.
///
/// ## Send + Sync refactor
///
/// The sync `SerialMonitor` wraps its `WsSink`/`WsSource` halves in
/// `std::sync::Mutex`, which cannot be held across `.await`. For the async
/// surface we switch to `Arc<tokio::sync::Mutex<Option<_>>>`:
///
/// * `tokio::sync::Mutex` — safe to hold across `.await` points, which
///   we do when calling `sink.send(...).await` / `source.next().await`.
/// * `Arc<...>` — lets us `clone` the handle into `async move { ... }`
///   blocks handed to `future_into_py`, so each future owns its own
///   reference into the shared connection state.
/// * `Option<_>` — the WS halves are absent before `__aenter__` and
///   after `__aexit__`, and the outer `Arc<Mutex<Option<_>>>` stays
///   alive across both transitions.
/// * Read/write halves live in **separate** mutexes so a pending
///   `read_lines` does not serialize an unrelated `write` (each direction
///   of the WebSocket has independent framing state).
///
/// The sync `SerialMonitor` is untouched — this is a purely additive
/// surface.
///
/// ```python
/// import asyncio
/// from fbuild._native import AsyncSerialMonitor
///
/// async def main():
///     async with AsyncSerialMonitor(port="COM13", baud_rate=115200) as mon:
///         lines = await mon.read_lines(timeout_secs=5.0)
///         await mon.write("hello\n")
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
    ws_write: Arc<tokio::sync::Mutex<Option<WsSink>>>,
    ws_read: Arc<tokio::sync::Mutex<Option<WsSource>>>,
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
            ws_write: Arc::new(tokio::sync::Mutex::new(None)),
            ws_read: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Async context-manager entry. Connects to the daemon's
    /// `/ws/serial-monitor` endpoint, sends the `attach` handshake, and
    /// stores the split sink/source halves for subsequent `read_lines` /
    /// `write` calls. Mirrors the sync `SerialMonitor::__enter__` contract.
    fn __aenter__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let port = slf.port.clone();
        let baud_rate = slf.baud_rate;
        let client_id = slf.client_id.clone();
        let verbose = slf.verbose;
        let ws_write_slot = slf.ws_write.clone();
        let ws_read_slot = slf.ws_read.clone();
        let slf_obj: PyObject = slf.into_py(py);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let daemon_port = fbuild_paths::get_daemon_port();
            let ws_url = format!("ws://127.0.0.1:{}/ws/serial-monitor", daemon_port);

            let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyConnectionError::new_err(format!(
                        "failed to connect to daemon WebSocket at {}: {}",
                        ws_url, e
                    ))
                })?;

            let (mut write, mut read) = ws_stream.split();

            let attach = ClientMessage::Attach {
                client_id: client_id.clone(),
                port: port.clone(),
                baud_rate,
                open_if_needed: true,
                pre_acquire_writer: true,
            };
            let attach_json = serde_json::to_string(&attach).unwrap();

            write
                .send(tungstenite::Message::Text(attach_json))
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyConnectionError::new_err(format!(
                        "failed to send attach: {}",
                        e
                    ))
                })?;

            let msg = read
                .next()
                .await
                .ok_or_else(|| {
                    pyo3::exceptions::PyConnectionError::new_err("WebSocket closed before attach")
                })?
                .map_err(|e| {
                    pyo3::exceptions::PyConnectionError::new_err(format!("WebSocket error: {}", e))
                })?;

            if let tungstenite::Message::Text(text) = msg {
                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(ServerMessage::Attached { success, .. }) if success => {
                        if verbose {
                            eprintln!("attached to {} at {} baud", port, baud_rate);
                        }
                    }
                    Ok(ServerMessage::Error { message }) => {
                        return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "attach failed: {}",
                            message
                        )));
                    }
                    _ => {
                        return Err(pyo3::exceptions::PyRuntimeError::new_err(
                            "unexpected response to attach",
                        ));
                    }
                }
            }

            *ws_write_slot.lock().await = Some(write);
            *ws_read_slot.lock().await = Some(read);

            Ok(slf_obj)
        })
    }

    /// Async context-manager exit. Sends a `detach` + `Close` frame and
    /// clears the stored sink/source halves. Mirrors sync `__exit__`.
    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: Option<PyObject>,
        _exc_val: Option<PyObject>,
        _exc_tb: Option<PyObject>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ws_write_slot = self.ws_write.clone();
        let ws_read_slot = self.ws_read.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            if let Some(mut write) = ws_write_slot.lock().await.take() {
                let detach = serde_json::to_string(&ClientMessage::Detach).unwrap();
                let _ = write.send(tungstenite::Message::Text(detach)).await;
                let _ = write.send(tungstenite::Message::Close(None)).await;
            }
            let _ = ws_read_slot.lock().await.take();
            Ok(false)
        })
    }

    /// Async counterpart to `SerialMonitor::read_lines`. Pulls batches of
    /// lines from the daemon until at least one batch arrives or the
    /// timeout elapses. Handles `Preempted` / `Reconnected` transparently
    /// when `auto_reconnect=true` just like the sync path.
    #[pyo3(signature = (timeout_secs=30.0))]
    fn read_lines<'py>(&self, py: Python<'py>, timeout_secs: f64) -> PyResult<Bound<'py, PyAny>> {
        let ws_read_slot = self.ws_read.clone();
        let auto_reconnect = self.auto_reconnect;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(read_lines_async(ws_read_slot, auto_reconnect, timeout_secs).await)
        })
    }

    /// Async counterpart to `SerialMonitor::write`. Returns `true` on
    /// successful delivery (daemon acknowledged with `write_ack`),
    /// `false` otherwise. Mirrors the sync contract except the return
    /// type is a bool rather than `bytes_written` to match the async
    /// signature spec in FastLED/fbuild#65.
    fn write<'py>(&self, py: Python<'py>, data: &str) -> PyResult<Bound<'py, PyAny>> {
        let ws_write_slot = self.ws_write.clone();
        let ws_read_slot = self.ws_read.clone();
        let encoded = encode_payload(data);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(write_async(ws_write_slot, ws_read_slot, encoded).await)
        })
    }

    /// Async counterpart to `SerialMonitor::write_json_rpc`. Serializes
    /// `request` to JSON, sends it with a trailing newline, then polls
    /// `read_lines` until a `REMOTE:` response arrives or the full
    /// `timeout_secs` elapses. Honors the PR #57 guarantee that an empty
    /// batch does not short-circuit the overall deadline.
    ///
    /// Returns the JSON-decoded response on success, raises
    /// `TimeoutError` on timeout.
    #[pyo3(signature = (request, timeout_secs=5.0))]
    fn write_json_rpc<'py>(
        &self,
        py: Python<'py>,
        request: &Bound<'_, PyAny>,
        timeout_secs: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        // Serialize the request on the calling thread (needs the GIL)
        // before we enter the async block, mirroring the send_op_async
        // pattern of moving only owned primitives across the .await
        // boundary.
        let json_str: String = py
            .import_bound("json")?
            .call_method1("dumps", (request,))?
            .extract()?;
        let data = format!("{}\n", json_str);
        let encoded = encode_payload(&data);

        let ws_write_slot = self.ws_write.clone();
        let ws_read_slot = self.ws_read.clone();
        let auto_reconnect = self.auto_reconnect;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // Fire-and-acknowledge the write first; we don't abort on
            // a write failure because the sync surface also proceeds
            // to poll for a REMOTE: response (which is how the Python
            // tests exercise this path).
            let _ = write_async(ws_write_slot, ws_read_slot.clone(), encoded).await;

            let json_part =
                wait_for_remote_json_rpc_response_async(timeout_secs, ws_read_slot, auto_reconnect)
                    .await;

            match json_part {
                Some(payload) => Python::with_gil(|py| {
                    let json_module = py.import_bound("json")?;
                    let parsed = json_module.call_method1("loads", (payload.trim(),))?;
                    Ok(parsed.unbind())
                }),
                None => Err(pyo3::exceptions::PyTimeoutError::new_err(format!(
                    "no REMOTE: response within {} seconds",
                    timeout_secs
                ))),
            }
        })
    }

    /// Asynchronously reset the device via the daemon's `POST /api/reset`
    /// endpoint. Returns `True` if the daemon reported success.
    #[pyo3(signature = (board=None))]
    fn reset_device<'py>(
        &self,
        py: Python<'py>,
        board: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let url = format!("{}/api/reset", fbuild_paths::get_daemon_url());
        let port = self.port.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            #[derive(Serialize)]
            struct ResetPayload {
                port: String,
                #[serde(skip_serializing_if = "Option::is_none")]
                board: Option<String>,
            }

            let payload = ResetPayload { port, board };

            let resp = reqwest::Client::new()
                .post(&url)
                .json(&payload)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyConnectionError::new_err(format!(
                        "failed to send reset request to daemon: {}",
                        e
                    ))
                })?;

            let body: serde_json::Value = resp.json().await.map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "failed to parse reset response: {}",
                    e
                ))
            })?;

            Ok(body
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false))
        })
    }
}
