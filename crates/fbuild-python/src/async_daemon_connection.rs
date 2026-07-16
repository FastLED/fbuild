//! Asynchronous `AsyncDaemonConnection` PyO3 binding — native async
//! counterpart to `DaemonConnection` (FastLED/fbuild#65).

use pyo3::prelude::*;

use crate::outcome::{
    OpRequest, build_url, deploy_url, monitor_url, outcome_to_pydict, platformio_src_dir_from_env,
    send_op_async,
};

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
pub(crate) struct AsyncDaemonConnection {
    project_dir: String,
    environment: String,
}

#[pymethods]
impl AsyncDaemonConnection {
    #[new]
    pub(crate) fn new(project_dir: String, environment: String) -> Self {
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
            Python::attach(|py| {
                let obj = Py::new(
                    py,
                    AsyncDaemonConnection {
                        project_dir,
                        environment,
                    },
                )?;
                Ok(obj.into_any())
            })
        })
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: Option<Py<PyAny>>,
        _exc_val: Option<Py<PyAny>>,
        _exc_tb: Option<Py<PyAny>>,
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
            Python::attach(|py| Ok(outcome_to_pydict(py, &outcome)?.unbind()))
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
            Python::attach(|py| Ok(outcome_to_pydict(py, &outcome)?.unbind()))
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
            Python::attach(|py| Ok(outcome_to_pydict(py, &outcome)?.unbind()))
        })
    }
}

impl AsyncDaemonConnection {
    pub(crate) fn build_request(&self, clean: bool, verbose: bool) -> OpRequest {
        OpRequest {
            project_dir: self.project_dir.clone(),
            environment: Some(self.environment.clone()),
            clean_build: clean,
            verbose,
            port: None,
            monitor_after: false,
            skip_build: false,
            baud_rate: None,
            src_dir: platformio_src_dir_from_env(),
        }
    }

    pub(crate) fn deploy_request(
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
            src_dir: platformio_src_dir_from_env(),
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
            src_dir: None,
        }
    }
}
