//! Synchronous `SerialMonitor` PyO3 binding — the API FastLED depends on.
//!
//! A thin facade over the shared [`crate::serial_session::SerialSession`]
//! core (FastLED/fbuild#1485). Every blocking call releases the GIL
//! (`py.detach`) and runs `rt.block_on(session.op(...))`.

use pyo3::prelude::*;
use tokio::runtime::Runtime;

use crate::serial_session::{SerialSession, SessionConfig, SessionError, SessionStatus};

fn map_err(err: SessionError) -> PyErr {
    match err {
        SessionError::Timeout => pyo3::exceptions::PyTimeoutError::new_err(err.to_string()),
        SessionError::Closed | SessionError::PortGone | SessionError::Preempted => {
            pyo3::exceptions::PyConnectionError::new_err(err.to_string())
        }
        SessionError::ProtocolDesync => pyo3::exceptions::PyRuntimeError::new_err(err.to_string()),
    }
}

/// Python-visible SerialMonitor class.
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
    client_id: String,
    // FastLED/fbuild#844: borrow the process-shared runtime rather than
    // constructing a fresh one per monitor session.
    runtime: Option<&'static Runtime>,
    session: Option<SerialSession>,
    last_line: std::sync::Mutex<String>,
}

impl SerialMonitor {
    fn config(&self) -> SessionConfig {
        let daemon_port = fbuild_paths::get_daemon_port();
        SessionConfig {
            ws_url: format!("ws://127.0.0.1:{daemon_port}/ws/serial-monitor"),
            port: self.port.clone(),
            baud_rate: self.baud_rate,
            auto_reconnect: self.auto_reconnect,
            verbose: self.verbose,
            client_id: self.client_id.clone(),
            max_buffered_lines: crate::serial_session::DEFAULT_MAX_BUFFERED_LINES,
            handshake_timeout: std::time::Duration::from_secs(5),
        }
    }

    /// `Err` if called from inside the shared tokio runtime (§4.9 rule 5):
    /// `block_on` would otherwise panic instead of deadlocking silently.
    fn block_on<T>(&self, fut: impl std::future::Future<Output = T>) -> PyResult<T> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "SerialMonitor called from inside the async runtime; use AsyncSerialMonitor",
            ));
        }
        let rt = self.runtime.ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("SerialMonitor runtime is not active")
        })?;
        Ok(rt.block_on(fut))
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
            client_id: uuid::Uuid::new_v4().to_string(),
            runtime: None,
            session: None,
            last_line: std::sync::Mutex::new(String::new()),
        }
    }

    fn __enter__<'py>(
        mut slf: PyRefMut<'py, Self>,
        py: Python<'py>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let rt: &'static Runtime = pyo3_async_runtimes::tokio::get_runtime();
        slf.runtime = Some(rt);
        let cfg = slf.config();
        let session = py
            .detach(|| rt.block_on(SerialSession::connect(cfg)))
            .map_err(map_err)?;
        slf.session = Some(session);
        Ok(slf)
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &mut self,
        py: Python<'_>,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> bool {
        if let (Some(session), Some(rt)) = (self.session.take(), self.runtime) {
            py.detach(|| rt.block_on(session.close()));
        }
        self.runtime = None;
        false
    }

    #[getter]
    fn last_line(&self) -> String {
        self.last_line
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Iterate over serial output lines: a list of lines received within
    /// the timeout period. Takes `&self`, so `write` can run from another
    /// thread while a read is in flight (FastLED/fbuild#1431).
    #[pyo3(signature = (timeout=30.0))]
    fn read_lines(&self, py: Python<'_>, timeout: f64) -> Vec<String> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let Some(rt) = self.runtime else {
            return Vec::new();
        };
        let lines = py.detach(|| {
            rt.block_on(session.read_lines(std::time::Duration::from_secs_f64(timeout.max(0.0))))
        });

        if let Some(last) = lines.last() {
            *self.last_line.lock().unwrap_or_else(|e| e.into_inner()) = last.clone();
        }
        if !self.hooks.is_empty() && !lines.is_empty() {
            for line in &lines {
                for hook in &self.hooks {
                    let _ = hook.call1(py, (line,));
                }
            }
        }
        lines
    }

    /// Wakes every blocked sync reader; each returns `[]` without draining,
    /// so queued lines stay for the next reader. Additive API for
    /// abandoning a blocked read from another thread (FastLED #3219).
    fn interrupt_reads(&self) {
        if let Some(session) = &self.session {
            session.interrupt_reads();
        }
    }

    /// Write data to the serial port. Releases the GIL while waiting for
    /// the daemon's `write_ack`; does not wait for an in-flight
    /// `read_lines` to finish (FastLED/fbuild#1431). Returns `0` on
    /// failure, matching the historical contract (see `write_json_rpc` /
    /// docs for the async surface's differing, raising, contract).
    fn write(&self, py: Python<'_>, data: &str) -> usize {
        let Some(session) = &self.session else {
            return 0;
        };
        let Some(rt) = self.runtime else {
            return 0;
        };
        py.detach(|| rt.block_on(session.write(data.as_bytes(), std::time::Duration::from_secs(5))))
            .unwrap_or(0)
    }

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
        }
        Ok(false)
    }

    /// Send a JSON-RPC request and wait for the matching `REMOTE:` response.
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

        let Some(session) = &self.session else {
            return Err(pyo3::exceptions::PyConnectionError::new_err(
                "SerialMonitor session is not open",
            ));
        };
        let reply = self.block_on(session.json_rpc(
            &json_str,
            std::time::Duration::from_secs_f64(timeout.max(0.0)),
        ))?;
        let reply = py.detach(|| reply);
        let json_part = reply.map_err(map_err)?;

        let json_module = py.import("json")?;
        let result = json_module.call_method1("loads", (json_part.trim(),))?;
        Ok(result.unbind())
    }

    /// Number of buffered serial lines not yet drained via `read_lines()`.
    /// Maps to pyserial's `Serial.in_waiting`. Returns 0 when the session
    /// is not open.
    #[getter]
    fn in_waiting(&self, py: Python<'_>) -> usize {
        let Some(session) = &self.session else {
            return 0;
        };
        let Some(rt) = self.runtime else {
            return 0;
        };
        py.detach(|| rt.block_on(session.in_waiting(std::time::Duration::from_secs(2))))
            .unwrap_or(0)
    }

    /// Drop any buffered serial-line data. Matches pyserial's
    /// `Serial.reset_input_buffer()`. No-op when the session is not open.
    fn reset_input_buffer(&self, py: Python<'_>) {
        let Some(session) = &self.session else {
            return;
        };
        let Some(rt) = self.runtime else {
            return;
        };
        py.detach(|| rt.block_on(session.clear_input()));
    }

    /// Reset the device via the daemon's DTR/RTS reset endpoint.
    ///
    /// Works whether or not `__enter__` has been called — the reset goes
    /// through the daemon's HTTP API, not the WebSocket session. Exclusive
    /// (`&mut self`): PyO3 raises `RuntimeError: Already borrowed` for an
    /// overlapping lifecycle call instead of racing.
    #[pyo3(signature = (board=None, wait_for_output=false, timeout=5.0))]
    fn reset_device(
        &mut self,
        py: Python<'_>,
        board: Option<String>,
        wait_for_output: bool,
        timeout: f64,
    ) -> PyResult<bool> {
        let port = self.port.clone();
        let success = match self.runtime {
            Some(rt) => py.detach(|| {
                rt.block_on(crate::async_serial_monitor::post_reset_request_async(
                    port, board,
                ))
            })?,
            None => {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| {
                        pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "failed to build tokio runtime: {e}"
                        ))
                    })?;
                py.detach(|| {
                    rt.block_on(crate::async_serial_monitor::post_reset_request_async(
                        port, board,
                    ))
                })?
            }
        };

        let was_connected = self.session.is_some();
        if was_connected {
            if let (Some(session), Some(rt)) = (self.session.take(), self.runtime) {
                py.detach(|| rt.block_on(session.close()));
            }
            if success && self.auto_reconnect {
                if let Some(rt) = self.runtime {
                    let cfg = self.config();
                    let session = py
                        .detach(|| rt.block_on(SerialSession::connect(cfg)))
                        .map_err(map_err)?;
                    self.session = Some(session);
                }
            }
        }

        if !success || !wait_for_output {
            return Ok(success);
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
        std::thread::sleep(std::time::Duration::from_millis(300));

        if self.session.is_some() {
            while std::time::Instant::now() < deadline {
                let remaining = (deadline - std::time::Instant::now())
                    .as_secs_f64()
                    .min(0.2);
                let lines = self.read_lines(py, remaining);
                if !lines.is_empty() {
                    return Ok(true);
                }
            }
            return Ok(false);
        }

        let wait = timeout.min(1.0);
        std::thread::sleep(std::time::Duration::from_secs_f64(wait));
        Ok(true)
    }
}

/// Whether the underlying session reports an active connection. Test-only
/// accessor kept crate-private; the Python API never sees session status
/// directly (§4.8 maps everything through exceptions).
#[allow(dead_code)]
pub(crate) fn status_is_active(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Active)
}
