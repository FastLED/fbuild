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

#![allow(dead_code, clippy::useless_conversion)]

use pyo3::prelude::*;

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
    // TODO: hold tokio runtime + WebSocket connection
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
        let _ = hooks; // TODO: store hooks for callback dispatch
        Self {
            port,
            baud_rate,
            auto_reconnect,
            verbose,
        }
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        // TODO: connect to daemon WebSocket, attach to port
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> bool {
        // TODO: detach from port, close WebSocket
        false
    }

    /// Iterate over serial output lines.
    ///
    /// Returns a list of lines received within the timeout period.
    /// In the Python API this is a generator; here we return a list
    /// that the Python side can iterate over.
    #[pyo3(signature = (timeout=30.0))]
    fn read_lines(&self, timeout: f64) -> Vec<String> {
        let _ = timeout;
        // TODO: poll WebSocket for "data" messages
        vec![]
    }

    /// Write data to the serial port.
    fn write(&self, data: &str) -> usize {
        let _ = data;
        // TODO: send "write" message via WebSocket
        0
    }

    /// Send a JSON-RPC request and wait for matching response.
    #[pyo3(signature = (request, timeout=5.0))]
    fn write_json_rpc(&self, request: &Bound<'_, PyAny>, timeout: f64) -> PyResult<PyObject> {
        let _ = (request, timeout);
        // TODO: serialize request, send via write, wait for REMOTE: response
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "write_json_rpc not yet implemented",
        ))
    }
}

/// Python-visible Daemon class (high-level API).
#[pyclass]
struct Daemon;

#[pymethods]
impl Daemon {
    #[staticmethod]
    fn ensure_running() -> bool {
        // TODO: HTTP GET to daemon /health, spawn if not running
        false
    }

    #[staticmethod]
    fn stop() -> bool {
        // TODO: HTTP POST to /api/daemon/shutdown
        false
    }

    #[staticmethod]
    fn status() -> PyResult<PyObject> {
        // TODO: HTTP GET to /api/daemon/info
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "status not yet implemented",
        ))
    }
}

/// Python-visible DaemonConnection (context manager).
#[pyclass]
struct DaemonConnection {
    project_dir: String,
    environment: String,
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
        let _ = (clean, verbose, timeout);
        // TODO: HTTP POST to /api/operations/build
        false
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
        let _ = (port, clean, skip_build, monitor_after, timeout);
        // TODO: HTTP POST to /api/operations/deploy
        false
    }

    #[pyo3(signature = (port=None, baud_rate=None, timeout=None))]
    fn monitor(&self, port: Option<String>, baud_rate: Option<u32>, timeout: Option<f64>) -> bool {
        let _ = (port, baud_rate, timeout);
        // TODO: HTTP POST to /api/operations/monitor
        false
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
