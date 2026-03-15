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
use std::sync::Mutex;
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
                    rt.block_on(tokio::time::timeout(remaining, read.next()))
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
        match rt.block_on(tokio::time::timeout(timeout, read.next())) {
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

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);

        while std::time::Instant::now() < deadline {
            let remaining = (deadline - std::time::Instant::now()).as_secs_f64();
            // read_lines takes &mut self but we only have &self here —
            // use the raw WS read directly
            let lines = self.read_lines_inner(remaining.min(1.0));
            for line in &lines {
                if let Some(json_part) = line.strip_prefix("REMOTE:") {
                    let json_module = py.import_bound("json")?;
                    let result = json_module.call_method1("loads", (json_part.trim(),))?;
                    return Ok(result.to_object(py));
                }
            }
            if lines.is_empty() {
                break;
            }
        }

        Err(pyo3::exceptions::PyTimeoutError::new_err(format!(
            "no REMOTE: response within {} seconds",
            timeout
        )))
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
                rt.block_on(tokio::time::timeout(remaining, read.next()))
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

#[derive(Serialize)]
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
        let url = format!("{}/api/build", fbuild_paths::get_daemon_url());
        let req = OpRequest {
            project_dir: self.project_dir.clone(),
            environment: Some(self.environment.clone()),
            clean_build: clean,
            verbose,
            port: None,
            monitor_after: false,
            skip_build: false,
            baud_rate: None,
        };
        send_op(&url, &req, timeout)
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
        let url = format!("{}/api/deploy", fbuild_paths::get_daemon_url());
        let req = OpRequest {
            project_dir: self.project_dir.clone(),
            environment: Some(self.environment.clone()),
            clean_build: clean,
            verbose: false,
            port,
            monitor_after,
            skip_build,
            baud_rate: None,
        };
        send_op(&url, &req, timeout)
    }

    #[pyo3(signature = (port=None, baud_rate=None, timeout=None))]
    fn monitor(&self, port: Option<String>, baud_rate: Option<u32>, timeout: Option<f64>) -> bool {
        let url = format!("{}/api/monitor", fbuild_paths::get_daemon_url());
        let req = OpRequest {
            project_dir: self.project_dir.clone(),
            environment: Some(self.environment.clone()),
            clean_build: false,
            verbose: false,
            port,
            monitor_after: false,
            skip_build: false,
            baud_rate,
        };
        send_op(&url, &req, timeout.unwrap_or(1800.0))
    }
}

fn send_op(url: &str, req: &OpRequest, timeout: f64) -> bool {
    let client = reqwest::blocking::Client::new();
    match client
        .post(url)
        .json(req)
        .timeout(std::time::Duration::from_secs_f64(timeout))
        .send()
    {
        Ok(resp) => {
            if let Ok(body) = resp.json::<serde_json::Value>() {
                body.get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

/// Factory function matching `from fbuild import connect_daemon`.
#[pyfunction]
fn connect_daemon(project_dir: String, environment: String) -> DaemonConnection {
    DaemonConnection::new(project_dir, environment)
}

/// The fbuild Python module.
#[pymodule]
fn fbuild(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", "2.0.0")?;
    m.add_class::<SerialMonitor>()?;
    m.add_class::<Daemon>()?;
    m.add_class::<DaemonConnection>()?;
    m.add_function(wrap_pyfunction!(connect_daemon, m)?)?;
    Ok(())
}
