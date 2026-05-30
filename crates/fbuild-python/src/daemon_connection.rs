//! Synchronous `DaemonConnection` PyO3 binding.

use pyo3::prelude::*;

use crate::outcome::{
    build_url, deploy_url, monitor_url, outcome_to_pydict, platformio_src_dir_from_env, send_op,
    OpRequest,
};

/// Python-visible DaemonConnection (context manager).
#[pyclass]
pub(crate) struct DaemonConnection {
    project_dir: String,
    environment: String,
}

#[pymethods]
impl DaemonConnection {
    #[new]
    pub(crate) fn new(project_dir: String, environment: String) -> Self {
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
