//! Synchronous `Daemon` and asynchronous `AsyncDaemon` PyO3 bindings.

use std::path::{Path, PathBuf};

use pyo3::prelude::*;

/// Filename of the daemon binary on this platform.
///
/// Windows ships `fbuild-daemon.exe`; Unix-like systems ship `fbuild-daemon`.
/// Kept as a single constant so the venv-adjacent lookup and the PATH
/// fallback agree on the name.
#[cfg(windows)]
const DAEMON_BIN_NAME: &str = "fbuild-daemon.exe";
#[cfg(not(windows))]
const DAEMON_BIN_NAME: &str = "fbuild-daemon";

/// Look for `fbuild-daemon[.exe]` next to `sys.executable` (FastLED/fbuild#275).
///
/// When `fbuild-python` is imported from a venv whose `Scripts/` (Windows) or
/// `bin/` (Unix) directory is NOT at the front of `PATH`, the bare
/// `Command::new("fbuild-daemon")` spawn picks up a stale user-level binary
/// (e.g. `~/.local/bin/fbuild-daemon.exe`) instead of the one shipped with the
/// venv. That mismatch can produce wrong builds — the stale daemon may miss
/// features the in-process `_native` extension depends on (e.g. `.S` source
/// support exposed in FastLED's `library.json` srcFilter).
///
/// Strategy: query `sys.executable` via the GIL, look for a sibling daemon
/// binary, return its absolute path if present. The caller falls back to the
/// PATH-relative `DAEMON_BIN_NAME` when this returns `None`, preserving the
/// previous behavior in non-venv installs.
fn venv_adjacent_daemon() -> Option<PathBuf> {
    Python::with_gil(|py| {
        let sys = py.import_bound("sys").ok()?;
        let exe_obj = sys.getattr("executable").ok()?;
        let exe_str: String = exe_obj.extract().ok()?;
        if exe_str.is_empty() {
            return None;
        }
        let exe_path = PathBuf::from(exe_str);
        let dir = exe_path.parent()?;
        daemon_in_dir(dir)
    })
}

/// Return the absolute path to `fbuild-daemon[.exe]` in `dir` if the file
/// exists. Split out from `venv_adjacent_daemon` so the resolution rule can
/// be exercised in unit tests without spinning up a Python interpreter.
fn daemon_in_dir(dir: &Path) -> Option<PathBuf> {
    let candidate = dir.join(DAEMON_BIN_NAME);
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

/// Build the spawn target for the daemon: prefer the venv-adjacent absolute
/// path (`Some`) and fall back to the PATH-relative bare name (`None`).
///
/// Returning an `Option<PathBuf>` rather than always materializing a
/// `PathBuf` keeps the PATH search semantics intact when no venv-adjacent
/// binary is found — passing a bare `"fbuild-daemon"` to `Command::new`
/// triggers the OS-level executable search, which is the legacy behavior
/// we must preserve for `pip install --user` / system installs.
fn daemon_spawn_target() -> Option<PathBuf> {
    venv_adjacent_daemon()
}

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
        //
        // Prefer the daemon binary sitting next to `sys.executable`
        // (FastLED/fbuild#275) so a venv install never gets shadowed by a
        // stale user-level daemon on PATH.
        let mut cmd = match daemon_spawn_target() {
            Some(path) => std::process::Command::new(path),
            None => std::process::Command::new(DAEMON_BIN_NAME),
        };
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
        // Resolve `sys.executable` BEFORE entering the future: the GIL
        // must not be held across `.await`, and the venv-adjacent lookup
        // is cheap (one attribute read + one stat).
        let spawn_target = daemon_spawn_target();

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
            // Venv-adjacent preference (FastLED/fbuild#275): resolved
            // synchronously above the future so we never touch the GIL
            // from inside this async block.
            // allow-direct-spawn: daemon must outlive the Python interpreter.
            let mut cmd = match spawn_target {
                Some(path) => std::process::Command::new(path),
                None => std::process::Command::new(DAEMON_BIN_NAME),
            };
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

#[cfg(test)]
mod tests {
    //! Tests for the venv-adjacent daemon resolution (FastLED/fbuild#275).
    //!
    //! We exercise `daemon_in_dir` rather than `venv_adjacent_daemon`
    //! because the latter reads `sys.executable` at runtime — the
    //! resolution rule (filename + `is_file()` check) is what we own and
    //! what the bug hinged on. Mocking the Python interpreter just to
    //! re-test stdlib attribute access would dilute the signal.
    use super::{daemon_in_dir, DAEMON_BIN_NAME};
    use std::fs;

    #[test]
    fn daemon_bin_name_matches_platform() {
        // The lookup file name must agree with what gets installed by
        // maturin / pip — on Windows that is `fbuild-daemon.exe`, on
        // Unix it is unsuffixed.
        #[cfg(windows)]
        assert_eq!(DAEMON_BIN_NAME, "fbuild-daemon.exe");
        #[cfg(not(windows))]
        assert_eq!(DAEMON_BIN_NAME, "fbuild-daemon");
    }

    #[test]
    fn daemon_in_dir_returns_path_when_file_present() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let target = tmp.path().join(DAEMON_BIN_NAME);
        fs::write(&target, b"dummy").expect("write dummy daemon");

        let resolved = daemon_in_dir(tmp.path()).expect("must find venv daemon");
        assert_eq!(resolved, target);
        assert!(
            resolved.is_absolute(),
            "resolved daemon path must be absolute so Command::new bypasses PATH"
        );
    }

    #[test]
    fn daemon_in_dir_returns_none_when_file_absent() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        // Deliberately do not create any file — this mimics a venv whose
        // scripts dir exists but does not ship the daemon binary
        // (e.g. partial install, dev checkout without maturin build).
        assert!(daemon_in_dir(tmp.path()).is_none());
    }

    #[test]
    fn daemon_in_dir_ignores_directory_with_matching_name() {
        // `is_file()` must reject the case where a directory shares the
        // daemon's name — otherwise we'd hand Command::new a non-
        // executable path and lose the legitimate PATH fallback.
        let tmp = tempfile::tempdir().expect("create tempdir");
        fs::create_dir(tmp.path().join(DAEMON_BIN_NAME)).expect("mkdir");
        assert!(daemon_in_dir(tmp.path()).is_none());
    }

    #[test]
    fn daemon_in_dir_returns_none_for_nonexistent_dir() {
        // Defensive: if `sys.executable.parent()` ever points somewhere
        // that no longer exists (e.g. a deleted venv), the lookup must
        // gracefully fall back to PATH rather than surface an error.
        let tmp = tempfile::tempdir().expect("create tempdir");
        let missing = tmp.path().join("does-not-exist");
        assert!(daemon_in_dir(&missing).is_none());
    }
}
