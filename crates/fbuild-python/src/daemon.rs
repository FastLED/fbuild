//! Synchronous `Daemon` and asynchronous `AsyncDaemon` PyO3 bindings.

use pyo3::prelude::*;

/// Python-visible Daemon class (high-level API).
#[pyclass]
pub(crate) struct Daemon;

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
pub(crate) struct AsyncDaemon;

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
