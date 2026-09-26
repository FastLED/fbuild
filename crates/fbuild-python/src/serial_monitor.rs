//! Synchronous `SerialMonitor` PyO3 binding — the API FastLED depends on.

use base64::Engine;
use futures::{SinkExt, StreamExt};
use pyo3::prelude::*;
use std::collections::VecDeque;
use std::sync::Mutex;
use tokio::runtime::Runtime;
use tokio_tungstenite::tungstenite;

use crate::json_rpc::wait_for_remote_json_rpc_response;
use crate::messages::{ClientMessage, ServerMessage, WsSink, WsSource};
use crate::ws_session::{ReadYield, WsSession};

/// Python-visible SerialMonitor class.
///
/// This is the critical binding that FastLED depends on.
/// It wraps the Rust SharedSerialManager via WebSocket,
/// matching the original Python API:
///
/// ```python
/// with SerialMonitor(port="COM13", baud_rate=115200) as mon:
///     for line in mon.read_lines(timeout=30.0):
///         print(line)
///     mon.write("hello\n")
/// ```
#[pyclass]
pub(crate) struct SerialMonitor {
    port: String,
    baud_rate: u32,
    auto_reconnect: bool,
    verbose: bool,
    hooks: Vec<Py<PyAny>>,
    // FastLED/fbuild#844: avoid `Runtime::new()` outside main/tests by
    // borrowing the process-shared `pyo3_async_runtimes::tokio` runtime.
    // Stored as `Option<&'static Runtime>` so the existing
    // `Some(rt)` session-active guards keep their semantics — `None`
    // means "before __enter__ / after __exit__", `Some(_)` means "WS
    // session is live".
    runtime: Option<&'static Runtime>,
    ws_write: Option<Mutex<WsSink>>,
    ws_read: Option<Mutex<WsSource>>,
    pending_lines: Mutex<VecDeque<String>>,
    /// Lets `write`/`in_waiting` take the WebSocket read half from an
    /// in-flight `read_lines` (FastLED/fbuild#1431).
    read_yield: ReadYield,
    client_id: String,
    last_line: Mutex<String>,
    #[allow(dead_code)]
    preempted: bool,
}

impl SerialMonitor {
    fn connect_ws(&self, rt: &Runtime) -> PyResult<(WsSink, WsSource)> {
        let daemon_port = fbuild_paths::get_daemon_port();
        let ws_url = format!("ws://127.0.0.1:{}/ws/serial-monitor", daemon_port);

        // FastLED/fbuild#810: cap the WebSocket handshake at 5s so a daemon
        // that accepts the TCP socket but never completes the WS upgrade
        // cannot hang FastLED's `with SerialMonitor(...)` forever. The
        // attach send + attach reply each get their own 5s deadline.
        const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

        let connect_result = rt.block_on(async {
            tokio::time::timeout(HANDSHAKE_TIMEOUT, tokio_tungstenite::connect_async(&ws_url)).await
        });
        let (ws_stream, _) = match connect_result {
            Ok(Ok(ok)) => ok,
            Ok(Err(e)) => {
                return Err(pyo3::exceptions::PyConnectionError::new_err(format!(
                    "failed to connect to daemon WebSocket at {}: {}",
                    ws_url, e
                )));
            }
            Err(_) => {
                return Err(pyo3::exceptions::PyConnectionError::new_err(format!(
                    "daemon WebSocket handshake at {} timed out after 5s",
                    ws_url
                )));
            }
        };

        let (mut write, mut read) = ws_stream.split();

        let attach = ClientMessage::Attach {
            client_id: self.client_id.clone(),
            port: self.port.clone(),
            baud_rate: self.baud_rate,
            open_if_needed: true,
            pre_acquire_writer: true,
            client_metadata: Some(crate::messages::ClientMetadata::current()),
        };
        let attach_json = serde_json::to_string(&attach)
            .expect("fbuild-python: ClientMessage::Attach serialization is infallible");

        let send_result = rt.block_on(async {
            tokio::time::timeout(
                HANDSHAKE_TIMEOUT,
                write.send(tungstenite::Message::Text(attach_json)),
            )
            .await
        });
        match send_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return Err(pyo3::exceptions::PyConnectionError::new_err(format!(
                    "failed to send attach: {}",
                    e
                )));
            }
            Err(_) => {
                return Err(pyo3::exceptions::PyConnectionError::new_err(
                    "daemon did not accept attach frame within 5s",
                ));
            }
        }

        let read_result =
            rt.block_on(async { tokio::time::timeout(HANDSHAKE_TIMEOUT, read.next()).await });
        let msg: tungstenite::Message = match read_result {
            Ok(Some(Ok(msg))) => msg,
            Ok(Some(Err(e))) => {
                return Err(pyo3::exceptions::PyConnectionError::new_err(format!(
                    "WebSocket error: {}",
                    e
                )));
            }
            Ok(None) => {
                return Err(pyo3::exceptions::PyConnectionError::new_err(
                    "WebSocket closed before attach",
                ));
            }
            Err(_) => {
                return Err(pyo3::exceptions::PyConnectionError::new_err(
                    "daemon did not reply to attach within 5s",
                ));
            }
        };

        if let tungstenite::Message::Text(text) = msg {
            match serde_json::from_str::<ServerMessage>(&text) {
                Ok(ServerMessage::Attached { success, .. }) if success => {
                    if self.verbose {
                        eprintln!("attached to {} at {} baud", self.port, self.baud_rate);
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

        Ok((write, read))
    }

    fn close_ws(&mut self) {
        if let (Some(rt), Some(ws_write)) = (&self.runtime, &self.ws_write) {
            let detach = serde_json::to_string(&ClientMessage::Detach)
                .expect("fbuild-python: ClientMessage::Detach serialization is infallible");
            if let Ok(mut write) = ws_write.lock() {
                let _ = rt.block_on(write.send(tungstenite::Message::Text(detach)));
                let _ = rt.block_on(write.send(tungstenite::Message::Close(None)));
            }
        }
        self.ws_write = None;
        self.ws_read = None;
        self.clear_pending_lines();
    }

    fn reconnect_ws(&mut self) -> PyResult<()> {
        let rt = self.runtime.as_ref().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("SerialMonitor runtime is not active")
        })?;
        let (write, read) = self.connect_ws(rt)?;
        self.ws_write = Some(Mutex::new(write));
        self.ws_read = Some(Mutex::new(read));
        Ok(())
    }

    /// The live WebSocket session, or `None` before `__enter__` / after
    /// `__exit__`.
    fn session(&self) -> Option<WsSession<'_>> {
        Some(WsSession {
            rt: self.runtime?,
            write: self.ws_write.as_ref()?,
            read: self.ws_read.as_ref()?,
            pending: &self.pending_lines,
            read_yield: &self.read_yield,
        })
    }

    /// Serial lines within `timeout`, without hook dispatch. Call with the
    /// GIL released.
    fn read_session_lines(&self, timeout: f64) -> Vec<String> {
        self.session().map_or_else(Vec::new, |session| {
            session.read_lines(
                std::time::Duration::from_secs_f64(timeout.max(0.0)),
                self.auto_reconnect,
            )
        })
    }

    /// Write `data` and wait for its `write_ack`. Call with the GIL released.
    fn write_session(&self, data: &str) -> usize {
        let Some(session) = self.session() else {
            return 0;
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(data.as_bytes());
        let msg = serde_json::to_string(&ClientMessage::Write { data: encoded })
            .expect("fbuild-python: ClientMessage::Write serialization is infallible");
        session
            .request(
                msg,
                std::time::Duration::from_secs(5),
                |reply| match reply {
                    ServerMessage::WriteAck {
                        success,
                        bytes_written,
                        ..
                    } => Some(if success { bytes_written } else { 0 }),
                    ServerMessage::Error { .. }
                    | ServerMessage::PortDisconnected { .. }
                    | ServerMessage::PortRebindFailed { .. } => Some(0),
                    _ => None,
                },
            )
            .unwrap_or(0)
    }

    fn pending_line_count(&self) -> usize {
        self.pending_lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    fn clear_pending_lines(&self) {
        self.pending_lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

#[pymethods]
impl SerialMonitor {
    #[new]
    #[pyo3(signature = (port, baud_rate=115200, hooks=None, auto_reconnect=true, verbose=false))]
    fn new(
        port: String,
        baud_rate: u32,
        hooks: Option<Vec<Py<PyAny>>>,
        auto_reconnect: bool,
        verbose: bool,
    ) -> Self {
        Self {
            port,
            baud_rate,
            auto_reconnect,
            verbose,
            hooks: hooks.unwrap_or_default(),
            runtime: None,
            ws_write: None,
            ws_read: None,
            pending_lines: Mutex::new(VecDeque::new()),
            read_yield: ReadYield::default(),
            client_id: uuid::Uuid::new_v4().to_string(),
            last_line: Mutex::new(String::new()),
            preempted: false,
        }
    }

    fn __enter__(mut slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        // FastLED/fbuild#844: borrow the process-shared async runtime
        // instead of constructing a fresh `tokio::runtime::Runtime` per
        // monitor session. The pyo3-async-runtimes runtime is the same
        // one the AsyncSerialMonitor binding already uses, so we don't
        // pay the overhead of spinning up a dedicated multi-threaded
        // pool every time FastLED enters a `with SerialMonitor(...)`
        // block, and the new lint ban on bare `Runtime::new()` outside
        // main/tests is satisfied.
        let rt: &'static Runtime = pyo3_async_runtimes::tokio::get_runtime();

        let (write, read) = slf.connect_ws(rt)?;
        slf.ws_write = Some(Mutex::new(write));
        slf.ws_read = Some(Mutex::new(read));
        slf.clear_pending_lines();
        slf.runtime = Some(rt);
        Ok(slf)
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &mut self,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> bool {
        self.close_ws();
        self.runtime = None;
        false
    }

    /// The last line received from the serial port.
    #[getter]
    fn last_line(&self) -> String {
        self.last_line
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Iterate over serial output lines.
    ///
    /// Returns a list of lines received within the timeout period.
    ///
    /// Takes `&self`, so `write` can run from another thread while a read
    /// is in flight; the read hands the socket to the write and resumes
    /// (FastLED/fbuild#1431).
    #[pyo3(signature = (timeout=30.0))]
    fn read_lines(&self, py: Python<'_>, timeout: f64) -> Vec<String> {
        let lines = py.detach(|| self.read_session_lines(timeout));

        // Update last_line and dispatch hooks
        if let Some(last) = lines.last() {
            *self.last_line.lock().unwrap_or_else(|e| e.into_inner()) = last.clone();
        }

        // Dispatch hooks for each line
        if !self.hooks.is_empty() && !lines.is_empty() {
            for line in &lines {
                for hook in &self.hooks {
                    let _ = hook.call1(py, (line,));
                }
            }
        }

        lines
    }

    /// Write data to the serial port.
    ///
    /// Releases the GIL while waiting for the daemon's `write_ack`, and does
    /// not wait for an in-flight `read_lines` to finish (FastLED/fbuild#1431).
    fn write(&self, py: Python<'_>, data: &str) -> usize {
        py.detach(|| self.write_session(data))
    }

    /// Run monitor until condition returns True or timeout expires.
    ///
    /// Calls `condition(line)` for each received line. Returns True if
    /// the condition was met, False on timeout.
    #[pyo3(signature = (condition, timeout=30.0))]
    fn run_until(&self, py: Python<'_>, condition: Py<PyAny>, timeout: f64) -> PyResult<bool> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);

        while std::time::Instant::now() < deadline {
            let remaining = (deadline - std::time::Instant::now()).as_secs_f64();
            if remaining <= 0.0 {
                break;
            }
            let lines = self.read_lines(py, remaining.min(1.0));
            for line in &lines {
                let result: bool = condition.call1(py, (line,))?.extract(py)?;
                if result {
                    return Ok(true);
                }
            }
            if lines.is_empty() {
                // Timeout on read_lines, check overall deadline
                continue;
            }
        }

        Ok(false)
    }

    /// Send a JSON-RPC request and wait for matching response.
    #[pyo3(signature = (request, timeout=5.0))]
    fn write_json_rpc(
        &self,
        py: Python<'_>,
        request: &Bound<'_, PyAny>,
        timeout: f64,
    ) -> PyResult<Py<PyAny>> {
        let json_str: String = py
            .import("json")?
            .call_method1("dumps", (request,))?
            .extract()?;

        let data = format!("{}\n", json_str);
        let reply = py.detach(|| {
            self.write_session(&data);
            wait_for_remote_json_rpc_response(timeout, |remaining| {
                self.read_session_lines(remaining.min(1.0))
            })
        });

        if let Some(json_part) = reply {
            let json_module = py.import("json")?;
            let result = json_module.call_method1("loads", (json_part.trim(),))?;
            return Ok(result.unbind());
        }

        Err(pyo3::exceptions::PyTimeoutError::new_err(format!(
            "no REMOTE: response within {} seconds",
            timeout
        )))
    }

    /// Number of buffered serial lines the daemon has produced for this
    /// session but the client has not yet drained via `read_lines()`.
    ///
    /// Maps to pyserial's `Serial.in_waiting`, modulo a units difference:
    /// the daemon brokers serial data as already-split lines, not raw
    /// bytes, so this is a line count. Returns 0 when the WebSocket
    /// session is not open (i.e. before `__enter__` or after `__exit__`).
    ///
    /// FastLED/fbuild#605 — added as part of the deprecation of direct
    /// pyserial use by fbuild clients.
    #[getter]
    fn in_waiting(&self, py: Python<'_>) -> usize {
        py.detach(|| {
            let Some(session) = self.session() else {
                return 0;
            };
            let msg = serde_json::to_string(&ClientMessage::GetInWaiting)
                .expect("fbuild-python: ClientMessage::GetInWaiting serialization is infallible");
            // Other messages (streaming Data frames, Preempted notifications)
            // can arrive in front of the reply; `request` keeps the lines and
            // waits for the typed answer until the 2s deadline expires.
            let count = session.request(
                msg,
                std::time::Duration::from_secs(2),
                |reply| match reply {
                    ServerMessage::InWaiting { count } => Some(count),
                    _ => None,
                },
            );
            count.map_or(0, |count| self.pending_line_count() + count)
        })
    }

    /// Drop any serial-line data the daemon has buffered for this
    /// session. Matches pyserial's `Serial.reset_input_buffer()`. No-op
    /// when the WebSocket session is not open.
    ///
    /// FastLED/fbuild#605 — added as part of the deprecation of direct
    /// pyserial use by fbuild clients.
    fn reset_input_buffer(&self) {
        self.clear_pending_lines();
        let Some(session) = self.session() else {
            return;
        };
        let msg = serde_json::to_string(&ClientMessage::ClearBuffer)
            .expect("fbuild-python: ClientMessage::ClearBuffer serialization is infallible");
        session.send(msg);
    }

    /// Reset the device via the daemon's DTR/RTS reset endpoint.
    ///
    /// Sends POST /api/reset to the daemon, which preempts any active
    /// serial monitor session, toggles DTR/RTS to reset the device,
    /// then clears preemption so monitors can reconnect.
    ///
    /// Works whether or not `__enter__` has been called — the reset goes
    /// through the daemon's HTTP API, not the WebSocket session.
    ///
    /// Args:
    ///     board: Board identifier (e.g. "esp32s3", "teensy40").
    ///            Determines the platform-specific reset sequence.
    ///            If None, a generic DTR toggle is used.
    ///     wait_for_output: If True, block until serial output is detected
    ///            after the reset (device has rebooted and is producing data).
    ///            If False (default), return immediately after reset.
    ///     timeout: Maximum seconds to wait for output (only used when
    ///            wait_for_output is True). Default: 5.0.
    ///
    /// Returns:
    ///     True if reset succeeded (and output detected, if wait_for_output).
    ///     False on failure or timeout.
    #[pyo3(signature = (board=None, wait_for_output=false, timeout=5.0))]
    fn reset_device(
        &mut self,
        board: Option<String>,
        wait_for_output: bool,
        timeout: f64,
    ) -> PyResult<bool> {
        // FastLED/fbuild#817: delegate the HTTP transport to the shared
        // async helper. `reset_device` may be called WITHOUT `__enter__`
        // (no WebSocket session), so `self.runtime` may be `None` — fall
        // back to a one-shot current-thread runtime in that case.
        let port = self.port.clone();
        let success = match self.runtime.as_ref() {
            Some(rt) => rt.block_on(crate::async_serial_monitor::post_reset_request_async(
                port, board,
            ))?,
            None => {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| {
                        pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "failed to build tokio runtime: {}",
                            e
                        ))
                    })?;
                rt.block_on(crate::async_serial_monitor::post_reset_request_async(
                    port, board,
                ))?
            }
        };

        let was_connected =
            self.runtime.is_some() && self.ws_write.is_some() && self.ws_read.is_some();
        if was_connected {
            self.close_ws();
            if success && self.auto_reconnect {
                self.reconnect_ws()?;
            }
        }

        if !success || !wait_for_output {
            return Ok(success);
        }

        // Wait for the device to produce serial output after reset.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);

        // Brief pause for USB re-enumeration after DTR toggle
        std::thread::sleep(std::time::Duration::from_millis(300));

        // If WebSocket is connected (__enter__ was called), poll via read_lines.
        // Note: the daemon preempts our session during reset and sends a
        // "Reconnected" message after. With auto_reconnect=true the WebSocket
        // transparently re-attaches, so read_session_lines will see new output.
        if self.runtime.is_some() && self.ws_read.is_some() {
            while std::time::Instant::now() < deadline {
                let remaining = (deadline - std::time::Instant::now())
                    .as_secs_f64()
                    .min(0.2);
                let lines = self.read_session_lines(remaining);
                if !lines.is_empty() {
                    return Ok(true);
                }
            }
            return Ok(false);
        }

        // No WebSocket — we can't observe output directly.
        // Wait a conservative 1 second (ESP32-S3 USB-CDC typically boots
        // in <500ms). The caller can pass a shorter timeout if needed.
        let wait = timeout.min(1.0);
        std::thread::sleep(std::time::Duration::from_secs_f64(wait));
        Ok(true)
    }
}
