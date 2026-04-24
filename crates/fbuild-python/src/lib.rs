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

#![allow(clippy::useless_conversion)]

use base64::Engine;
use futures::{SinkExt, StreamExt};
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;
use tokio_tungstenite::tungstenite;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures::stream::SplitSink<WsStream, tungstenite::Message>;
type WsSource = futures::stream::SplitStream<WsStream>;

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
struct SerialMonitor {
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

/// Messages we receive from the daemon (subset we care about).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Attached {
        success: bool,
        #[allow(dead_code)]
        message: String,
        #[allow(dead_code)]
        writer_pre_acquired: bool,
    },
    Data {
        lines: Vec<String>,
        #[allow(dead_code)]
        current_index: u64,
    },
    WriteAck {
        #[allow(dead_code)]
        success: bool,
        bytes_written: usize,
        #[allow(dead_code)]
        message: Option<String>,
    },
    Preempted {
        #[allow(dead_code)]
        reason: String,
        #[allow(dead_code)]
        preempted_by: String,
    },
    Reconnected {
        #[allow(dead_code)]
        message: String,
    },
    Error {
        message: String,
    },
    #[serde(other)]
    Other,
}

/// Client message to send to the daemon.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Attach {
        client_id: String,
        port: String,
        baud_rate: u32,
        open_if_needed: bool,
        pre_acquire_writer: bool,
    },
    Write {
        data: String,
    },
    Detach,
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

        // Connect to daemon WebSocket
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

        // Send attach message
        let attach = ClientMessage::Attach {
            client_id: slf.client_id.clone(),
            port: slf.port.clone(),
            baud_rate: slf.baud_rate,
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

        // Wait for attached response
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
                    if slf.verbose {
                        eprintln!("attached to {} at {} baud", slf.port, slf.baud_rate);
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
        if let (Some(ref rt), Some(ref ws_write)) = (&self.runtime, &self.ws_write) {
            let detach = serde_json::to_string(&ClientMessage::Detach).unwrap();
            if let Ok(mut write) = ws_write.lock() {
                let _ = rt.block_on(write.send(tungstenite::Message::Text(detach)));
                let _ = rt.block_on(write.send(tungstenite::Message::Close(None)));
            }
        }
        self.ws_write = None;
        self.ws_read = None;
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
        &self,
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

fn extract_remote_json_rpc_response(lines: &[String]) -> Option<String> {
    lines.iter().find_map(|line| {
        line.strip_prefix("REMOTE:")
            .map(|json_part| json_part.to_string())
    })
}

fn wait_for_remote_json_rpc_response<F>(timeout: f64, mut poll: F) -> Option<String>
where
    F: FnMut(f64) -> Vec<String>,
{
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);

    while std::time::Instant::now() < deadline {
        let remaining = (deadline - std::time::Instant::now()).as_secs_f64();
        let lines = poll(remaining);
        if let Some(json_part) = extract_remote_json_rpc_response(&lines) {
            return Some(json_part);
        }
    }

    None
}

/// Python-visible Daemon class (high-level API).
#[pyclass]
struct Daemon;

#[pymethods]
impl Daemon {
    #[staticmethod]
    fn ensure_running() -> bool {
        let url = format!("{}/health", fbuild_paths::get_daemon_url());
        if let Ok(resp) = reqwest::blocking::get(&url) {
            if resp.status().is_success() {
                return true;
            }
        }

        // INTENTIONALLY DETACHED (FastLED/fbuild#32): the Python host
        // spawns the daemon and then the Python interpreter may exit —
        // the daemon must survive. This PyO3 binding runs inside the
        // Python interpreter process, which has no global containment
        // group, so `spawn()` is already uncontained; see the matching
        // comment in fbuild-cli/src/daemon_client.rs.
        // allow-direct-spawn: daemon must outlive the Python interpreter.
        let mut cmd = std::process::Command::new("fbuild-daemon");
        if fbuild_paths::is_dev_mode() {
            cmd.arg("--dev");
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        if cmd.spawn().is_err() {
            return false;
        }

        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(resp) = reqwest::blocking::get(&url) {
                if resp.status().is_success() {
                    return true;
                }
            }
        }
        false
    }

    #[staticmethod]
    fn stop() -> bool {
        let url = format!("{}/api/daemon/shutdown", fbuild_paths::get_daemon_url());
        reqwest::blocking::Client::new()
            .post(&url)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    #[staticmethod]
    fn status(py: Python<'_>) -> PyResult<PyObject> {
        let url = format!("{}/api/daemon/info", fbuild_paths::get_daemon_url());
        let resp = reqwest::blocking::get(&url).map_err(|e| {
            pyo3::exceptions::PyConnectionError::new_err(format!(
                "failed to connect to daemon: {}",
                e
            ))
        })?;
        let text = resp.text().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("failed to read response: {}", e))
        })?;
        let json_module = py.import_bound("json")?;
        let result = json_module.call_method1("loads", (text,))?;
        Ok(result.to_object(py))
    }
}

/// Python-visible DaemonConnection (context manager).
#[pyclass]
struct DaemonConnection {
    project_dir: String,
    environment: String,
}

#[derive(Clone, Serialize)]
struct OpRequest {
    project_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    clean_build: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    verbose: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    monitor_after: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    skip_build: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    baud_rate: Option<u32>,
}

#[pymethods]
impl DaemonConnection {
    #[new]
    fn new(project_dir: String, environment: String) -> Self {
        Self {
            project_dir,
            environment,
        }
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> bool {
        false
    }

    #[pyo3(signature = (clean=false, verbose=false, timeout=1800.0))]
    fn build(&self, clean: bool, verbose: bool, timeout: f64) -> bool {
        send_op(&build_url(), &self.build_request(clean, verbose), timeout).success
    }

    #[pyo3(signature = (port=None, clean=false, skip_build=false, monitor_after=false, timeout=1800.0))]
    fn deploy(
        &self,
        port: Option<String>,
        clean: bool,
        skip_build: bool,
        monitor_after: bool,
        timeout: f64,
    ) -> bool {
        send_op(
            &deploy_url(),
            &self.deploy_request(port, clean, skip_build, monitor_after),
            timeout,
        )
        .success
    }

    #[pyo3(signature = (port=None, baud_rate=None, timeout=None))]
    fn monitor(&self, port: Option<String>, baud_rate: Option<u32>, timeout: Option<f64>) -> bool {
        send_op(
            &monitor_url(),
            &self.monitor_request(port, baud_rate),
            timeout.unwrap_or(1800.0),
        )
        .success
    }

    /// Same as `build()` but returns a dict with structured result fields:
    /// `success`, `message`, `exit_code`, `stdout`, `stderr`. Callers that
    /// need to branch on failure mode can inspect the dict instead of
    /// swallowing a bare bool. See FastLED/fbuild#18.
    #[pyo3(signature = (clean=false, verbose=false, timeout=1800.0))]
    fn build_result<'py>(
        &self,
        py: Python<'py>,
        clean: bool,
        verbose: bool,
        timeout: f64,
    ) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
        let outcome = send_op(&build_url(), &self.build_request(clean, verbose), timeout);
        outcome_to_pydict(py, &outcome)
    }

    /// Structured-result counterpart to `deploy()`. See `build_result()`.
    #[pyo3(signature = (port=None, clean=false, skip_build=false, monitor_after=false, timeout=1800.0))]
    fn deploy_result<'py>(
        &self,
        py: Python<'py>,
        port: Option<String>,
        clean: bool,
        skip_build: bool,
        monitor_after: bool,
        timeout: f64,
    ) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
        let outcome = send_op(
            &deploy_url(),
            &self.deploy_request(port, clean, skip_build, monitor_after),
            timeout,
        );
        outcome_to_pydict(py, &outcome)
    }

    /// Structured-result counterpart to `monitor()`. See `build_result()`.
    #[pyo3(signature = (port=None, baud_rate=None, timeout=None))]
    fn monitor_result<'py>(
        &self,
        py: Python<'py>,
        port: Option<String>,
        baud_rate: Option<u32>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
        let outcome = send_op(
            &monitor_url(),
            &self.monitor_request(port, baud_rate),
            timeout.unwrap_or(1800.0),
        );
        outcome_to_pydict(py, &outcome)
    }
}

impl DaemonConnection {
    fn build_request(&self, clean: bool, verbose: bool) -> OpRequest {
        OpRequest {
            project_dir: self.project_dir.clone(),
            environment: Some(self.environment.clone()),
            clean_build: clean,
            verbose,
            port: None,
            monitor_after: false,
            skip_build: false,
            baud_rate: None,
        }
    }

    fn deploy_request(
        &self,
        port: Option<String>,
        clean: bool,
        skip_build: bool,
        monitor_after: bool,
    ) -> OpRequest {
        OpRequest {
            project_dir: self.project_dir.clone(),
            environment: Some(self.environment.clone()),
            clean_build: clean,
            verbose: false,
            port,
            monitor_after,
            skip_build,
            baud_rate: None,
        }
    }

    fn monitor_request(&self, port: Option<String>, baud_rate: Option<u32>) -> OpRequest {
        OpRequest {
            project_dir: self.project_dir.clone(),
            environment: Some(self.environment.clone()),
            clean_build: false,
            verbose: false,
            port,
            monitor_after: false,
            skip_build: false,
            baud_rate,
        }
    }
}

fn build_url() -> String {
    format!("{}/api/build", fbuild_paths::get_daemon_url())
}

fn deploy_url() -> String {
    format!("{}/api/deploy", fbuild_paths::get_daemon_url())
}

fn monitor_url() -> String {
    format!("{}/api/monitor", fbuild_paths::get_daemon_url())
}

/// Structured result of a daemon operation (build/deploy/monitor).
///
/// Used internally by `send_op` and exposed to Python callers via
/// `DaemonConnection::{build,deploy,monitor}_result`. Lets callers branch
/// on specific failure modes (transport error vs. build error vs. no
/// response) instead of inspecting a bare bool. See FastLED/fbuild#18.
#[derive(Debug, Clone, Default)]
struct OperationOutcome {
    success: bool,
    message: Option<String>,
    exit_code: Option<i32>,
    stdout: Option<String>,
    stderr: Option<String>,
}

fn outcome_to_pydict<'py>(
    py: Python<'py>,
    outcome: &OperationOutcome,
) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
    let dict = pyo3::types::PyDict::new_bound(py);
    dict.set_item("success", outcome.success)?;
    dict.set_item("message", outcome.message.clone())?;
    dict.set_item("exit_code", outcome.exit_code)?;
    dict.set_item("stdout", outcome.stdout.clone())?;
    dict.set_item("stderr", outcome.stderr.clone())?;
    Ok(dict)
}

fn parse_outcome(body: &serde_json::Value) -> OperationOutcome {
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

fn send_op(url: &str, req: &OpRequest, timeout: f64) -> OperationOutcome {
    let client = reqwest::blocking::Client::new();
    match client
        .post(url)
        .json(req)
        .timeout(std::time::Duration::from_secs_f64(timeout))
        .send()
    {
        Ok(resp) => match resp.json::<serde_json::Value>() {
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

/// Native-async counterpart to `send_op`. Issues the same HTTP POST against
/// the daemon but yields on I/O instead of blocking a thread, so callers on
/// an asyncio event loop don't need FastLED's `_run_in_thread` shim.
///
/// Returns the same `OperationOutcome` so the sync and async surfaces share
/// `parse_outcome` and `outcome_to_pydict`. See FastLED/fbuild#65.
async fn send_op_async(url: String, req: OpRequest, timeout: f64) -> OperationOutcome {
    let client = reqwest::Client::new();
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
struct AsyncSerialMonitor {
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
        let encoded = base64::engine::general_purpose::STANDARD.encode(data.as_bytes());

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
        let encoded = base64::engine::general_purpose::STANDARD.encode(data.as_bytes());

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

/// Shared async read-batch loop used by `AsyncSerialMonitor::read_lines`
/// and `write_json_rpc`. Acquires the source mutex only for the duration
/// of each `.next()` call so that concurrent `write` / `__aexit__`
/// futures can still progress between iterations.
async fn read_lines_async(
    ws_read_slot: Arc<tokio::sync::Mutex<Option<WsSource>>>,
    auto_reconnect: bool,
    timeout_secs: f64,
) -> Vec<String> {
    let mut lines = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout_secs);

    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;

        let mut guard = ws_read_slot.lock().await;
        let Some(source) = guard.as_mut() else {
            // Session not entered (or already exited). Nothing to read.
            break;
        };

        let result = tokio::time::timeout(remaining, source.next()).await;
        // Drop the guard before continuing the loop so another task
        // can take the mutex (e.g. __aexit__).
        drop(guard);

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
                    Ok(ServerMessage::Reconnected { .. }) => continue,
                    _ => continue,
                }
            }
            Ok(Some(Ok(tungstenite::Message::Close(_)))) | Ok(None) => break,
            Err(_) => break, // timeout
            _ => continue,
        }
    }

    lines
}

/// Shared async write path used by `AsyncSerialMonitor::write` and
/// `write_json_rpc`. Sends the already-base64-encoded payload, then
/// awaits a `write_ack` (with a 5-second bound matching the sync path).
/// Returns `true` if the ack reported any bytes written.
async fn write_async(
    ws_write_slot: Arc<tokio::sync::Mutex<Option<WsSink>>>,
    ws_read_slot: Arc<tokio::sync::Mutex<Option<WsSource>>>,
    encoded: String,
) -> bool {
    let msg = serde_json::to_string(&ClientMessage::Write { data: encoded }).unwrap();

    {
        let mut guard = ws_write_slot.lock().await;
        let Some(sink) = guard.as_mut() else {
            return false;
        };
        if sink.send(tungstenite::Message::Text(msg)).await.is_err() {
            return false;
        }
    }

    // Wait for write_ack. Hold the read half's mutex only across this
    // single `.next()` so concurrent `read_lines` futures can resume.
    let mut guard = ws_read_slot.lock().await;
    let Some(source) = guard.as_mut() else {
        return false;
    };
    let ack_timeout = std::time::Duration::from_secs(5);
    match tokio::time::timeout(ack_timeout, source.next()).await {
        Ok(Some(Ok(tungstenite::Message::Text(text)))) => {
            matches!(
                serde_json::from_str::<ServerMessage>(&text),
                Ok(ServerMessage::WriteAck { bytes_written, .. }) if bytes_written > 0
            )
        }
        _ => false,
    }
}

/// Async counterpart to `wait_for_remote_json_rpc_response`. Keeps
/// polling `read_lines_async` until the deadline expires, even if an
/// individual batch comes back empty — preserving the PR #57 fix.
async fn wait_for_remote_json_rpc_response_async(
    timeout_secs: f64,
    ws_read_slot: Arc<tokio::sync::Mutex<Option<WsSource>>>,
    auto_reconnect: bool,
) -> Option<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout_secs);

    while std::time::Instant::now() < deadline {
        let remaining = (deadline - std::time::Instant::now()).as_secs_f64();
        if remaining <= 0.0 {
            break;
        }
        let lines = read_lines_async(ws_read_slot.clone(), auto_reconnect, remaining).await;
        if let Some(json_part) = extract_remote_json_rpc_response(&lines) {
            return Some(json_part);
        }
    }

    None
}

/// Python-visible AsyncDaemon class.
///
/// Native async counterpart to `Daemon`. Follows the same additive
/// pattern as `AsyncSerialMonitor` (Issue #65): the sync `Daemon` class
/// stays unchanged, and this one exposes async methods so callers under
/// an asyncio event loop can `await` them directly.
///
/// ```python
/// import asyncio
/// from fbuild._native import AsyncDaemon
///
/// async def main():
///     info = await AsyncDaemon.status()
///
/// asyncio.run(main())
/// ```
#[pyclass]
struct AsyncDaemon;

#[pymethods]
impl AsyncDaemon {
    /// Asynchronously fetch `/api/daemon/info` from the daemon. Returns
    /// a JSON-deserialized Python object on success, or raises a
    /// ConnectionError/RuntimeError.
    #[staticmethod]
    fn status(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        let url = format!("{}/api/daemon/info", fbuild_paths::get_daemon_url());

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let resp = reqwest::Client::new()
                .get(&url)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyConnectionError::new_err(format!(
                        "failed to connect to daemon: {}",
                        e
                    ))
                })?;

            let text = resp.text().await.map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "failed to read daemon response: {}",
                    e
                ))
            })?;

            Python::with_gil(|py| {
                let json_module = py.import_bound("json")?;
                let parsed = json_module.call_method1("loads", (text,))?;
                Ok(parsed.unbind())
            })
        })
    }

    /// Asynchronously ensure the daemon is running. Mirrors the sync
    /// `Daemon.ensure_running` contract: returns `True` if the daemon
    /// responds to `/health`, spawning a new `fbuild-daemon` process if
    /// needed and polling until the health endpoint succeeds.
    ///
    /// The spawn itself is synchronous (`std::process::Command::spawn`)
    /// because `tokio::process::Command` adds no value for a detached
    /// child — the child does not need an async stdio pipe. The key win
    /// for async callers is that the health poll loop uses
    /// `tokio::time::sleep` and async reqwest instead of blocking the
    /// event loop thread.
    #[staticmethod]
    fn ensure_running(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        let url = format!("{}/health", fbuild_paths::get_daemon_url());
        let dev_mode = fbuild_paths::is_dev_mode();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let client = reqwest::Client::new();

            // Fast path: daemon is already up.
            if let Ok(resp) = client
                .get(&url)
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await
            {
                if resp.status().is_success() {
                    return Ok(true);
                }
            }

            // INTENTIONALLY DETACHED (FastLED/fbuild#32): see the
            // matching comment in `Daemon::ensure_running` above.
            // allow-direct-spawn: daemon must outlive the Python interpreter.
            let mut cmd = std::process::Command::new("fbuild-daemon");
            if dev_mode {
                cmd.arg("--dev");
            }
            cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());

            if cmd.spawn().is_err() {
                return Ok(false);
            }

            for _ in 0..100 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if let Ok(resp) = client
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await
                {
                    if resp.status().is_success() {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        })
    }

    /// Asynchronously shut down the daemon via `POST /api/daemon/shutdown`.
    /// Returns `True` if the daemon acknowledged with a 2xx response.
    #[staticmethod]
    fn stop(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        let url = format!("{}/api/daemon/shutdown", fbuild_paths::get_daemon_url());

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let ok = reqwest::Client::new()
                .post(&url)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            Ok(ok)
        })
    }
}

/// Python-visible AsyncDaemonConnection class.
///
/// Native async counterpart to `DaemonConnection`. Exposes `build`,
/// `deploy`, and `monitor` (and their `_result` variants) as async methods
/// that call the daemon over `reqwest::Client` (non-blocking) instead of
/// the blocking client used by the sync sibling. This is the method set
/// FastLED/fbuild#65 explicitly targets under "Daemon/DaemonConnection:
/// send_op and any other HTTP call".
///
/// ```python
/// import asyncio
/// from fbuild._native import AsyncDaemonConnection
///
/// async def main():
///     conn = AsyncDaemonConnection(project_dir="tests/platform/uno", environment="uno")
///     ok = await conn.build()
///     result = await conn.build_result()
///
/// asyncio.run(main())
/// ```
#[pyclass]
struct AsyncDaemonConnection {
    project_dir: String,
    environment: String,
}

#[pymethods]
impl AsyncDaemonConnection {
    #[new]
    fn new(project_dir: String, environment: String) -> Self {
        Self {
            project_dir,
            environment,
        }
    }

    /// Async context manager entry. Returns self so callers can
    /// `async with AsyncDaemonConnection(...) as conn:`.
    fn __aenter__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let project_dir = slf.project_dir.clone();
        let environment = slf.environment.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Python::with_gil(|py| {
                let obj = Py::new(
                    py,
                    AsyncDaemonConnection {
                        project_dir,
                        environment,
                    },
                )?;
                Ok(obj.to_object(py))
            })
        })
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: Option<PyObject>,
        _exc_val: Option<PyObject>,
        _exc_tb: Option<PyObject>,
    ) -> PyResult<Bound<'py, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(false) })
    }

    /// Async counterpart to `DaemonConnection::build`. Awaits the daemon's
    /// `POST /api/build` response and returns the `success` bool.
    #[pyo3(signature = (clean=false, verbose=false, timeout=1800.0))]
    fn build<'py>(
        &self,
        py: Python<'py>,
        clean: bool,
        verbose: bool,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let url = build_url();
        let req = self.build_request(clean, verbose);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(send_op_async(url, req, timeout).await.success)
        })
    }

    /// Async counterpart to `DaemonConnection::deploy`.
    #[pyo3(signature = (port=None, clean=false, skip_build=false, monitor_after=false, timeout=1800.0))]
    fn deploy<'py>(
        &self,
        py: Python<'py>,
        port: Option<String>,
        clean: bool,
        skip_build: bool,
        monitor_after: bool,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let url = deploy_url();
        let req = self.deploy_request(port, clean, skip_build, monitor_after);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(send_op_async(url, req, timeout).await.success)
        })
    }

    /// Async counterpart to `DaemonConnection::monitor`.
    #[pyo3(signature = (port=None, baud_rate=None, timeout=None))]
    fn monitor<'py>(
        &self,
        py: Python<'py>,
        port: Option<String>,
        baud_rate: Option<u32>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let url = monitor_url();
        let req = self.monitor_request(port, baud_rate);
        let t = timeout.unwrap_or(1800.0);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(send_op_async(url, req, t).await.success)
        })
    }

    /// Async counterpart to `DaemonConnection::build_result`. Returns the
    /// full structured outcome dict (`success`, `message`, `exit_code`,
    /// `stdout`, `stderr`) — matches the sync surface exactly.
    #[pyo3(signature = (clean=false, verbose=false, timeout=1800.0))]
    fn build_result<'py>(
        &self,
        py: Python<'py>,
        clean: bool,
        verbose: bool,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let url = build_url();
        let req = self.build_request(clean, verbose);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let outcome = send_op_async(url, req, timeout).await;
            Python::with_gil(|py| Ok(outcome_to_pydict(py, &outcome)?.unbind()))
        })
    }

    /// Async counterpart to `DaemonConnection::deploy_result`.
    #[pyo3(signature = (port=None, clean=false, skip_build=false, monitor_after=false, timeout=1800.0))]
    fn deploy_result<'py>(
        &self,
        py: Python<'py>,
        port: Option<String>,
        clean: bool,
        skip_build: bool,
        monitor_after: bool,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let url = deploy_url();
        let req = self.deploy_request(port, clean, skip_build, monitor_after);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let outcome = send_op_async(url, req, timeout).await;
            Python::with_gil(|py| Ok(outcome_to_pydict(py, &outcome)?.unbind()))
        })
    }

    /// Async counterpart to `DaemonConnection::monitor_result`.
    #[pyo3(signature = (port=None, baud_rate=None, timeout=None))]
    fn monitor_result<'py>(
        &self,
        py: Python<'py>,
        port: Option<String>,
        baud_rate: Option<u32>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let url = monitor_url();
        let req = self.monitor_request(port, baud_rate);
        let t = timeout.unwrap_or(1800.0);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let outcome = send_op_async(url, req, t).await;
            Python::with_gil(|py| Ok(outcome_to_pydict(py, &outcome)?.unbind()))
        })
    }
}

impl AsyncDaemonConnection {
    fn build_request(&self, clean: bool, verbose: bool) -> OpRequest {
        OpRequest {
            project_dir: self.project_dir.clone(),
            environment: Some(self.environment.clone()),
            clean_build: clean,
            verbose,
            port: None,
            monitor_after: false,
            skip_build: false,
            baud_rate: None,
        }
    }

    fn deploy_request(
        &self,
        port: Option<String>,
        clean: bool,
        skip_build: bool,
        monitor_after: bool,
    ) -> OpRequest {
        OpRequest {
            project_dir: self.project_dir.clone(),
            environment: Some(self.environment.clone()),
            clean_build: clean,
            verbose: false,
            port,
            monitor_after,
            skip_build,
            baud_rate: None,
        }
    }

    fn monitor_request(&self, port: Option<String>, baud_rate: Option<u32>) -> OpRequest {
        OpRequest {
            project_dir: self.project_dir.clone(),
            environment: Some(self.environment.clone()),
            clean_build: false,
            verbose: false,
            port,
            monitor_after: false,
            skip_build: false,
            baud_rate,
        }
    }
}

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
    use super::{
        extract_remote_json_rpc_response, parse_outcome, send_op_async,
        wait_for_remote_json_rpc_response, OpRequest, PYTHON_MODULE_VERSION,
    };
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
