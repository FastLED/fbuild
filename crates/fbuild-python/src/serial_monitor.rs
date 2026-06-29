//! Synchronous `SerialMonitor` PyO3 binding — the API FastLED depends on.

use base64::Engine;
use futures::{SinkExt, StreamExt};
use pyo3::prelude::*;
use serde::Serialize;
use std::sync::Mutex;
use tokio::runtime::Runtime;
use tokio_tungstenite::tungstenite;

use crate::json_rpc::wait_for_remote_json_rpc_response;
use crate::messages::{ClientMessage, ServerMessage, WsSink, WsSource};

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
    hooks: Vec<PyObject>,
    runtime: Option<Runtime>,
    ws_write: Option<Mutex<WsSink>>,
    ws_read: Option<Mutex<WsSource>>,
    client_id: String,
    last_line: String,
    #[allow(dead_code)]
    preempted: bool,
}

impl SerialMonitor {
    fn connect_ws(&self, rt: &Runtime) -> PyResult<(WsSink, WsSource)> {
        let daemon_port = fbuild_paths::get_daemon_port();
        let ws_url = format!("ws://127.0.0.1:{}/ws/serial-monitor", daemon_port);

        let (ws_stream, _) = rt
            .block_on(tokio_tungstenite::connect_async(&ws_url))
            .map_err(|e| {
                pyo3::exceptions::PyConnectionError::new_err(format!(
                    "failed to connect to daemon WebSocket at {}: {}",
                    ws_url, e
                ))
            })?;

        let (mut write, mut read) = ws_stream.split();

        let attach = ClientMessage::Attach {
            client_id: self.client_id.clone(),
            port: self.port.clone(),
            baud_rate: self.baud_rate,
            open_if_needed: true,
            pre_acquire_writer: true,
        };
        let attach_json = serde_json::to_string(&attach).unwrap();

        rt.block_on(write.send(tungstenite::Message::Text(attach_json)))
            .map_err(|e| {
                pyo3::exceptions::PyConnectionError::new_err(format!(
                    "failed to send attach: {}",
                    e
                ))
            })?;

        let msg: tungstenite::Message = rt
            .block_on(read.next())
            .ok_or_else(|| {
                pyo3::exceptions::PyConnectionError::new_err("WebSocket closed before attach")
            })?
            .map_err(|e| {
                pyo3::exceptions::PyConnectionError::new_err(format!("WebSocket error: {}", e))
            })?;

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
        if let (Some(ref rt), Some(ref ws_write)) = (&self.runtime, &self.ws_write) {
            let detach = serde_json::to_string(&ClientMessage::Detach).unwrap();
            if let Ok(mut write) = ws_write.lock() {
                let _ = rt.block_on(write.send(tungstenite::Message::Text(detach)));
                let _ = rt.block_on(write.send(tungstenite::Message::Close(None)));
            }
        }
        self.ws_write = None;
        self.ws_read = None;
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
}

#[pymethods]
impl SerialMonitor {
    #[new]
    #[pyo3(signature = (port, baud_rate=115200, hooks=None, auto_reconnect=true, verbose=false))]
    fn new(
        port: String,
        baud_rate: u32,
        hooks: Option<Vec<PyObject>>,
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
            client_id: uuid::Uuid::new_v4().to_string(),
            last_line: String::new(),
            preempted: false,
        }
    }

    fn __enter__(mut slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        let rt = Runtime::new().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("failed to create runtime: {}", e))
        })?;

        let (write, read) = slf.connect_ws(&rt)?;
        slf.ws_write = Some(Mutex::new(write));
        slf.ws_read = Some(Mutex::new(read));
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
    fn last_line(&self) -> &str {
        &self.last_line
    }

    /// Iterate over serial output lines.
    ///
    /// Returns a list of lines received within the timeout period.
    #[pyo3(signature = (timeout=30.0))]
    fn read_lines(&mut self, py: Python<'_>, timeout: f64) -> Vec<String> {
        let (Some(ref rt), Some(ref ws_read)) = (&self.runtime, &self.ws_read) else {
            return vec![];
        };

        let mut lines = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
        let auto_reconnect = self.auto_reconnect;

        py.allow_threads(|| {
            while std::time::Instant::now() < deadline {
                let remaining = deadline - std::time::Instant::now();
                let result = {
                    let mut read = ws_read.lock().unwrap();
                    // tokio::time::timeout MUST be constructed inside the
                    // runtime context, otherwise it panics with "there is
                    // no reactor running" because the Sleep future needs
                    // Handle::current() to register with the timer driver.
                    rt.block_on(async { tokio::time::timeout(remaining, read.next()).await })
                };

                match result {
                    Ok(Some(Ok(tungstenite::Message::Text(text)))) => {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(ServerMessage::Data {
                                lines: data_lines, ..
                            }) => {
                                lines.extend(data_lines);
                                if !lines.is_empty() {
                                    break;
                                }
                            }
                            Ok(ServerMessage::Preempted { .. }) => {
                                // Pause — deploy is happening
                                if auto_reconnect {
                                    continue;
                                }
                                break;
                            }
                            Ok(ServerMessage::Reconnected { .. }) => {
                                // Resume after deploy
                                continue;
                            }
                            Ok(ServerMessage::PortRenumbered { .. })
                            | Ok(ServerMessage::PortReattached { .. }) => continue,
                            Ok(ServerMessage::PortRebindFailed { .. }) => break,
                            Ok(ServerMessage::PortDisconnected { .. }) => break,
                            _ => continue,
                        }
                    }
                    Ok(Some(Ok(tungstenite::Message::Close(_)))) | Ok(None) => break,
                    Err(_) => break, // timeout
                    _ => continue,
                }
            }
        });

        // Update last_line and dispatch hooks
        if let Some(last) = lines.last() {
            self.last_line = last.clone();
        }

        // Dispatch hooks for each line
        if !self.hooks.is_empty() && !lines.is_empty() {
            Python::with_gil(|py| {
                for line in &lines {
                    for hook in &self.hooks {
                        let _ = hook.call1(py, (line,));
                    }
                }
            });
        }

        lines
    }

    /// Write data to the serial port.
    fn write(&self, data: &str) -> usize {
        let (Some(ref rt), Some(ref ws_write), Some(ref ws_read)) =
            (&self.runtime, &self.ws_write, &self.ws_read)
        else {
            return 0;
        };

        let encoded = base64::engine::general_purpose::STANDARD.encode(data.as_bytes());
        let msg = serde_json::to_string(&ClientMessage::Write { data: encoded }).unwrap();

        {
            let mut write = ws_write.lock().unwrap();
            if rt
                .block_on(write.send(tungstenite::Message::Text(msg)))
                .is_err()
            {
                return 0;
            }
        }

        // Wait for write_ack
        let mut read = ws_read.lock().unwrap();
        let timeout = std::time::Duration::from_secs(5);
        // tokio::time::timeout must be created inside the runtime context.
        match rt.block_on(async { tokio::time::timeout(timeout, read.next()).await }) {
            Ok(Some(Ok(tungstenite::Message::Text(text)))) => {
                if let Ok(ServerMessage::WriteAck { bytes_written, .. }) =
                    serde_json::from_str(&text)
                {
                    return bytes_written;
                }
                0
            }
            _ => 0,
        }
    }

    /// Run monitor until condition returns True or timeout expires.
    ///
    /// Calls `condition(line)` for each received line. Returns True if
    /// the condition was met, False on timeout.
    #[pyo3(signature = (condition, timeout=30.0))]
    fn run_until(&mut self, py: Python<'_>, condition: PyObject, timeout: f64) -> PyResult<bool> {
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
    ) -> PyResult<PyObject> {
        let json_str: String = py
            .import_bound("json")?
            .call_method1("dumps", (request,))?
            .extract()?;

        let data = format!("{}\n", json_str);
        self.write(&data);

        if let Some(json_part) = wait_for_remote_json_rpc_response(timeout, |remaining| {
            // read_lines takes &mut self but we only have &self here —
            // use the raw WS read directly.
            self.read_lines_inner(remaining.min(1.0))
        }) {
            let json_module = py.import_bound("json")?;
            let result = json_module.call_method1("loads", (json_part.trim(),))?;
            return Ok(result.to_object(py));
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
    fn in_waiting(&self) -> usize {
        let (Some(ref rt), Some(ref ws_write), Some(ref ws_read)) =
            (&self.runtime, &self.ws_write, &self.ws_read)
        else {
            return 0;
        };

        let msg = serde_json::to_string(&ClientMessage::GetInWaiting).unwrap();
        {
            let mut write = ws_write.lock().unwrap();
            if rt
                .block_on(write.send(tungstenite::Message::Text(msg)))
                .is_err()
            {
                return 0;
            }
        }

        // Read until we see an InWaiting reply. Other messages (e.g.
        // streaming Data frames or Preempted notifications) can arrive
        // in front of the reply; ignore them and keep waiting for the
        // typed answer until the 2s deadline expires.
        let mut read = ws_read.lock().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let remaining = deadline - std::time::Instant::now();
            let result = rt.block_on(async { tokio::time::timeout(remaining, read.next()).await });
            match result {
                Ok(Some(Ok(tungstenite::Message::Text(text)))) => {
                    if let Ok(ServerMessage::InWaiting { count }) =
                        serde_json::from_str::<ServerMessage>(&text)
                    {
                        return count;
                    }
                    continue;
                }
                Ok(Some(Ok(tungstenite::Message::Close(_)))) | Ok(None) => break,
                Err(_) => break,
                _ => continue,
            }
        }
        0
    }

    /// Drop any serial-line data the daemon has buffered for this
    /// session. Matches pyserial's `Serial.reset_input_buffer()`. No-op
    /// when the WebSocket session is not open.
    ///
    /// FastLED/fbuild#605 — added as part of the deprecation of direct
    /// pyserial use by fbuild clients.
    fn reset_input_buffer(&self) {
        let (Some(ref rt), Some(ref ws_write)) = (&self.runtime, &self.ws_write) else {
            return;
        };
        let msg = serde_json::to_string(&ClientMessage::ClearBuffer).unwrap();
        let mut write = ws_write.lock().unwrap();
        let _ = rt.block_on(write.send(tungstenite::Message::Text(msg)));
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
        let url = format!("{}/api/reset", fbuild_paths::get_daemon_url());

        #[derive(Serialize)]
        struct ResetPayload {
            port: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            board: Option<String>,
        }

        let payload = ResetPayload {
            port: self.port.clone(),
            board,
        };

        let resp = reqwest::blocking::Client::new()
            .post(&url)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .map_err(|e| {
                pyo3::exceptions::PyConnectionError::new_err(format!(
                    "failed to send reset request to daemon: {}",
                    e
                ))
            })?;

        let body: serde_json::Value = resp.json().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "failed to parse reset response: {}",
                e
            ))
        })?;

        let success = body
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

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
        // transparently re-attaches, so read_lines_inner will see new output.
        if self.runtime.is_some() && self.ws_read.is_some() {
            while std::time::Instant::now() < deadline {
                let remaining = (deadline - std::time::Instant::now())
                    .as_secs_f64()
                    .min(0.2);
                let lines = self.read_lines_inner(remaining);
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

impl SerialMonitor {
    /// Internal read_lines without hook dispatch (for write_json_rpc which has &self).
    fn read_lines_inner(&self, timeout: f64) -> Vec<String> {
        let (Some(ref rt), Some(ref ws_read)) = (&self.runtime, &self.ws_read) else {
            return vec![];
        };

        let mut lines = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
        let auto_reconnect = self.auto_reconnect;

        while std::time::Instant::now() < deadline {
            let remaining = deadline - std::time::Instant::now();
            let result = {
                let mut read = ws_read.lock().unwrap();
                // tokio::time::timeout must be created inside the runtime
                // context (otherwise: "there is no reactor running" panic).
                rt.block_on(async { tokio::time::timeout(remaining, read.next()).await })
            };

            match result {
                Ok(Some(Ok(tungstenite::Message::Text(text)))) => {
                    match serde_json::from_str::<ServerMessage>(&text) {
                        Ok(ServerMessage::Data {
                            lines: data_lines, ..
                        }) => {
                            lines.extend(data_lines);
                            if !lines.is_empty() {
                                break;
                            }
                        }
                        Ok(ServerMessage::Preempted { .. }) => {
                            if auto_reconnect {
                                continue;
                            }
                            break;
                        }
                        Ok(ServerMessage::Reconnected { .. }) => {
                            continue;
                        }
                        Ok(ServerMessage::PortRenumbered { .. })
                        | Ok(ServerMessage::PortReattached { .. }) => continue,
                        Ok(ServerMessage::PortRebindFailed { .. }) => break,
                        Ok(ServerMessage::PortDisconnected { .. }) => break,
                        _ => continue,
                    }
                }
                Ok(Some(Ok(tungstenite::Message::Close(_)))) | Ok(None) => break,
                Err(_) => break,
                _ => continue,
            }
        }

        lines
    }
}
