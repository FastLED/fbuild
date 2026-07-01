//! Synchronous `Daemon` and asynchronous `AsyncDaemon` PyO3 bindings.

use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use running_process::broker::adopt::{AdoptError, AsyncBrokerSession, OwnedConnectRequest};
use running_process::broker::client::RefusalKind;

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

fn shutdown_caller_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    insert_shutdown_header(
        &mut headers,
        "x-fbuild-client-pid",
        std::process::id().to_string(),
    );
    if let Ok(cwd) = std::env::current_dir() {
        insert_shutdown_header(
            &mut headers,
            "x-fbuild-client-cwd",
            cwd.to_string_lossy().into_owned(),
        );
    }
    if let Ok(exe) = std::env::current_exe() {
        insert_shutdown_header(
            &mut headers,
            "x-fbuild-client-exe",
            exe.to_string_lossy().into_owned(),
        );
    }
    insert_shutdown_header(
        &mut headers,
        "x-fbuild-client-argv",
        std::env::args().collect::<Vec<_>>().join(" "),
    );
    headers
}

fn insert_shutdown_header(
    headers: &mut reqwest::header::HeaderMap,
    name: &'static str,
    value: String,
) {
    if let Ok(value) = reqwest::header::HeaderValue::from_str(&value) {
        headers.insert(reqwest::header::HeaderName::from_static(name), value);
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

fn direct_health_url() -> String {
    format!("{}/health", fbuild_paths::get_daemon_url())
}

fn direct_info_url() -> String {
    format!("{}/api/daemon/info", fbuild_paths::get_daemon_url())
}

fn daemon_cache_identity_error(info: &serde_json::Value) -> Option<String> {
    let expected = fbuild_paths::running_process::DaemonCacheIdentity::discover();
    let expected_label = expected.label_value();
    if info.get("cache_identity").and_then(|v| v.as_str()) != Some(expected_label.as_str()) {
        return Some(format!(
            "broker negotiated fbuild-daemon with cache identity {:?}, expected {:?}",
            info.get("cache_identity").and_then(|v| v.as_str()),
            expected_label
        ));
    }
    let expected_schema = fbuild_paths::running_process::CACHE_SCHEMA_VERSION as u64;
    if info.get("cache_schema_version").and_then(|v| v.as_u64()) != Some(expected_schema) {
        return Some(format!(
            "broker negotiated fbuild-daemon with cache schema {:?}, expected {}",
            info.get("cache_schema_version").and_then(|v| v.as_u64()),
            expected_schema
        ));
    }
    None
}

/// Build a single-thread tokio runtime for one-shot sync helpers. Returns a
/// stringly-typed error so callers can plumb it through `Result<_, String>`
/// without taking on a tokio import. FastLED/fbuild#817.
fn one_shot_runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to build tokio runtime: {e}"))
}

async fn verify_broker_daemon_cache_identity_async() -> Result<(), String> {
    let info: serde_json::Value = fbuild_core::http::client()
        .get(direct_info_url())
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| format!("daemon info request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("daemon info response was invalid JSON: {e}"))?;
    if let Some(err) = daemon_cache_identity_error(&info) {
        return Err(err);
    }
    Ok(())
}

fn broker_endpoint() -> Option<String> {
    if fbuild_paths::running_process::running_process_disabled() {
        return None;
    }
    running_process::broker::doctor::default_broker_endpoint().ok()
}

async fn ensure_running_via_broker_async(url: &str) -> Result<bool, String> {
    let Some(endpoint) = broker_endpoint() else {
        return Ok(false);
    };
    let request = OwnedConnectRequest::new(
        endpoint,
        fbuild_paths::running_process::SERVICE_NAME,
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_VERSION"),
    );
    match AsyncBrokerSession::adopt(request).await {
        Ok(_session) => {
            let client = fbuild_core::http::client();
            for _ in 0..100 {
                if let Ok(resp) = client
                    .get(url)
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await
                {
                    if resp.status().is_success() {
                        verify_broker_daemon_cache_identity_async().await?;
                        return Ok(true);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(
                "broker negotiated fbuild-daemon, but its HTTP endpoint did not become healthy"
                    .to_string(),
            )
        }
        Err(AdoptError::BrokerDisabled) => Ok(false),
        Err(AdoptError::DisableEnv(err)) => Err(err.to_string()),
        Err(AdoptError::Connect(err)) => {
            if broker_refusal_is_fatal(err.refusal_kind()) {
                Err(format!(
                    "running-process broker refused fbuild daemon version: {err}"
                ))
            } else {
                Ok(false)
            }
        }
        Err(AdoptError::AsyncJoin(_)) => Ok(false),
    }
}

fn broker_refusal_is_fatal(kind: Option<RefusalKind>) -> bool {
    matches!(
        kind,
        Some(RefusalKind::VersionUnsupported | RefusalKind::VersionBlocked)
    )
}

/// Shared async implementation of `ensure_running`. Used by both the sync
/// `Daemon::ensure_running` (via a one-shot tokio runtime) and the async
/// `AsyncDaemon::ensure_running` (via `pyo3_async_runtimes::tokio`). The
/// process spawn must be resolved against the venv before this is called.
/// FastLED/fbuild#817.
async fn ensure_running_async_impl(
    url: &str,
    spawn_target: Option<PathBuf>,
    dev_mode: bool,
) -> bool {
    match ensure_running_via_broker_async(url).await {
        Ok(true) => return true,
        Ok(false) => {}
        Err(_) => return false,
    }

    let client = fbuild_core::http::client();

    // Fast path: daemon is already up.
    if let Ok(resp) = client
        .get(url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        if resp.status().is_success() {
            return true;
        }
    }

    // INTENTIONALLY DETACHED (FastLED/fbuild#32): the Python host spawns
    // the daemon and the Python interpreter may exit — the daemon must
    // survive. This PyO3 binding runs inside the Python interpreter
    // process, which has no global containment group, so `spawn()` is
    // already uncontained; see the matching comment in
    // `fbuild-cli/src/daemon_client.rs`.
    //
    // Prefer the daemon binary sitting next to `sys.executable`
    // (FastLED/fbuild#275) so a venv install never gets shadowed by a
    // stale user-level daemon on PATH.
    // allow-direct-spawn: daemon must outlive the Python interpreter.
    let mut cmd = match spawn_target {
        // allow-direct-spawn: daemon must outlive the Python interpreter.
        Some(path) => std::process::Command::new(path),
        // allow-direct-spawn: daemon must outlive the Python interpreter.
        None => std::process::Command::new(DAEMON_BIN_NAME),
    };
    if dev_mode {
        cmd.arg("--dev");
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    if cmd.spawn().is_err() {
        return false;
    }

    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Ok(resp) = client
            .get(url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            if resp.status().is_success() {
                return true;
            }
        }
    }
    false
}

/// Shared async implementation of `stop`. Returns `true` iff the daemon
/// responded 2xx to the shutdown POST. FastLED/fbuild#817.
async fn stop_async_impl() -> bool {
    let url = format!("{}/api/daemon/shutdown", fbuild_paths::get_daemon_url());
    fbuild_core::http::client()
        .post(&url)
        .headers(shutdown_caller_headers())
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Shared async implementation of `status`. Returns the response body text
/// (the caller decodes JSON with the GIL held). FastLED/fbuild#817.
async fn status_async_impl() -> PyResult<String> {
    let url = format!("{}/api/daemon/info", fbuild_paths::get_daemon_url());
    let resp = fbuild_core::http::client()
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

    resp.text().await.map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("failed to read response: {}", e))
    })
}

/// Python-visible Daemon class (high-level API).
#[pyclass]
pub(crate) struct Daemon;

#[pymethods]
impl Daemon {
    #[staticmethod]
    fn ensure_running() -> bool {
        // FastLED/fbuild#817: sync wrapper around the shared async impl.
        // Resolve `sys.executable` BEFORE entering the runtime — the
        // venv-adjacent lookup needs the GIL and must not be held across
        // `.await`.
        let url = direct_health_url();
        let dev_mode = fbuild_paths::is_dev_mode();
        let spawn_target = daemon_spawn_target();
        let Ok(rt) = one_shot_runtime() else {
            return false;
        };
        rt.block_on(ensure_running_async_impl(&url, spawn_target, dev_mode))
    }

    #[staticmethod]
    fn stop() -> bool {
        // FastLED/fbuild#817: sync wrapper around `stop_async_impl`.
        let Ok(rt) = one_shot_runtime() else {
            return false;
        };
        rt.block_on(stop_async_impl())
    }

    #[staticmethod]
    fn status(py: Python<'_>) -> PyResult<PyObject> {
        // FastLED/fbuild#817: sync wrapper around `status_async_impl`.
        let rt = one_shot_runtime().map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        let text = rt.block_on(status_async_impl())?;
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
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let text = status_async_impl().await?;
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
        let url = direct_health_url();
        let dev_mode = fbuild_paths::is_dev_mode();
        // Resolve `sys.executable` BEFORE entering the future: the GIL
        // must not be held across `.await`, and the venv-adjacent lookup
        // is cheap (one attribute read + one stat).
        let spawn_target = daemon_spawn_target();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(ensure_running_async_impl(&url, spawn_target, dev_mode).await)
        })
    }

    /// Asynchronously shut down the daemon via `POST /api/daemon/shutdown`.
    /// Returns `True` if the daemon acknowledged with a 2xx response.
    #[staticmethod]
    fn stop(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(stop_async_impl().await) })
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
    use super::{
        broker_refusal_is_fatal, daemon_cache_identity_error, daemon_in_dir, DAEMON_BIN_NAME,
    };
    use running_process::broker::client::RefusalKind;
    use std::fs;

    #[test]
    fn broker_version_refusals_are_fatal() {
        assert!(broker_refusal_is_fatal(Some(
            RefusalKind::VersionUnsupported
        )));
        assert!(broker_refusal_is_fatal(Some(RefusalKind::VersionBlocked)));
    }

    #[test]
    fn broker_non_refusal_errors_can_fallback() {
        assert!(!broker_refusal_is_fatal(None));
    }

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

    fn daemon_info_for_cache_identity(
        cache_identity: Option<String>,
        cache_schema_version: Option<u64>,
    ) -> serde_json::Value {
        let mut value = serde_json::json!({});
        if let Some(cache_identity) = cache_identity {
            value["cache_identity"] = serde_json::Value::String(cache_identity);
        }
        if let Some(cache_schema_version) = cache_schema_version {
            value["cache_schema_version"] = serde_json::Value::Number(cache_schema_version.into());
        }
        value
    }

    #[test]
    fn daemon_cache_identity_accepts_current_identity() {
        let identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
        let info = daemon_info_for_cache_identity(
            Some(identity.label_value()),
            Some(fbuild_paths::running_process::CACHE_SCHEMA_VERSION as u64),
        );

        assert!(daemon_cache_identity_error(&info).is_none());
    }

    #[test]
    fn daemon_cache_identity_rejects_missing_identity() {
        let info = daemon_info_for_cache_identity(
            None,
            Some(fbuild_paths::running_process::CACHE_SCHEMA_VERSION as u64),
        );

        let err = daemon_cache_identity_error(&info).expect("missing identity must fail closed");
        assert!(err.contains("cache identity"));
    }

    #[test]
    fn daemon_cache_identity_rejects_wrong_schema() {
        let identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
        let info = daemon_info_for_cache_identity(Some(identity.label_value()), Some(u64::MAX));

        let err = daemon_cache_identity_error(&info).expect("schema mismatch must fail closed");
        assert!(err.contains("cache schema"));
    }
}
