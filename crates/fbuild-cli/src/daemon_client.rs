//! HTTP client for communicating with the fbuild daemon.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use running_process::broker::adopt::{AdoptError, AsyncBrokerSession, OwnedConnectRequest};
use running_process::broker::client::RefusalKind;
use serde::Serialize;

mod types;
pub use types::*;

const LONG_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

/// Percent-encode a port name for use in a URL path segment.
fn encode_port(port: &str) -> String {
    port.replace('%', "%25").replace('/', "%2F")
}

fn stream_status_message(event: &StreamEvent) -> Option<String> {
    event
        .dependency_install
        .as_ref()
        .map(|status| match status.version.as_deref() {
            Some(version) => format!("{} {}: {}", status.name, version, status.message),
            None => format!("{}: {}", status.name, status.message),
        })
        .or_else(|| event.message.clone())
        .or_else(|| {
            event.current_operation.as_ref().map(|op| {
                if event.operation_in_progress.unwrap_or(false) {
                    format!("daemon busy: {}", op)
                } else {
                    op.clone()
                }
            })
        })
}

fn daemon_cache_identity_error(info: &DaemonInfoResponse) -> Option<String> {
    let expected = fbuild_paths::running_process::DaemonCacheIdentity::discover();
    let expected_label = expected.label_value();
    if info.cache_identity.as_deref() != Some(expected_label.as_str()) {
        return Some(format!(
            "broker negotiated fbuild-daemon with cache identity {:?}, expected {:?}",
            info.cache_identity.as_deref(),
            expected_label
        ));
    }
    let expected_schema = fbuild_paths::running_process::CACHE_SCHEMA_VERSION;
    if info.cache_schema_version != Some(expected_schema) {
        return Some(format!(
            "broker negotiated fbuild-daemon with cache schema {:?}, expected {}",
            info.cache_schema_version, expected_schema
        ));
    }
    None
}

/// Return the current process PID and working directory for request auditing.
pub fn caller_info() -> (Option<u32>, Option<String>) {
    let pid = Some(std::process::id());
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().to_string());
    (pid, cwd)
}

/// Snapshot all `PLATFORMIO_*` env vars from the current process environment.
///
/// This is the only place in the codebase where `std::env::vars()` is consulted
/// for `PLATFORMIO_*` keys (other than `fbuild-paths` startup fallbacks). The
/// returned map is forwarded to the daemon over HTTP via `BuildRequest.pio_env`
/// / `DeployRequest.pio_env`, since the daemon process does not inherit caller
/// env vars.
pub fn capture_pio_env() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| k.starts_with("PLATFORMIO_"))
        .collect()
}

pub fn runtime_diagnostic() -> String {
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());
    let daemon_exe = daemon_executable_hint();
    let running_process = last_daemon_acquisition()
        .map(|a| a.summary())
        .unwrap_or_else(|| {
            fbuild_paths::running_process::running_process_adoption_summary().to_string()
        });
    format!(
        "fbuild executable: {}\nfbuild version: {}\nfbuild-daemon executable: {}\ndaemon endpoint: {}\nrunning-process broker: {}",
        exe,
        env!("CARGO_PKG_VERSION"),
        daemon_exe,
        fbuild_paths::get_daemon_url(),
        running_process
    )
}

#[derive(Debug, Clone)]
pub enum DaemonAcquisition {
    BrokerNegotiated {
        endpoint: String,
        daemon_version: Option<String>,
    },
    DirectFallback {
        reason: String,
    },
    DisabledDirectFallback,
}

impl DaemonAcquisition {
    pub fn mode(&self) -> &'static str {
        match self {
            Self::BrokerNegotiated { .. } => "broker-negotiated",
            Self::DirectFallback { .. } => "direct-fallback",
            Self::DisabledDirectFallback => "disabled",
        }
    }

    pub fn endpoint(&self) -> Option<&str> {
        match self {
            Self::BrokerNegotiated { endpoint, .. } => Some(endpoint.as_str()),
            _ => None,
        }
    }

    pub fn daemon_version(&self) -> Option<&str> {
        match self {
            Self::BrokerNegotiated { daemon_version, .. } => daemon_version.as_deref(),
            _ => None,
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::DirectFallback { reason } => Some(reason.as_str()),
            _ => None,
        }
    }

    fn summary(&self) -> String {
        match self {
            Self::BrokerNegotiated {
                endpoint,
                daemon_version,
            } => format!(
                "broker-negotiated daemon{} at {}",
                daemon_version
                    .as_deref()
                    .map(|v| format!(" version {v}"))
                    .unwrap_or_default(),
                endpoint
            ),
            Self::DirectFallback { reason } => format!("direct daemon fallback ({reason})"),
            Self::DisabledDirectFallback => {
                "direct daemon fallback (RUNNING_PROCESS_DISABLE=1)".to_string()
            }
        }
    }
}

static LAST_DAEMON_ACQUISITION: OnceLock<Mutex<Option<DaemonAcquisition>>> = OnceLock::new();

fn record_daemon_acquisition(acquisition: DaemonAcquisition) {
    let slot = LAST_DAEMON_ACQUISITION.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(acquisition);
    }
}

pub fn last_daemon_acquisition() -> Option<DaemonAcquisition> {
    LAST_DAEMON_ACQUISITION
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|guard| guard.clone()))
}

fn daemon_executable_hint() -> String {
    let Some(parent) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    else {
        return "fbuild-daemon".to_string();
    };
    let stem = parent.join("fbuild-daemon");
    for candidate in [stem.clone(), stem.with_extension("exe")] {
        if candidate.exists() {
            return candidate.display().to_string();
        }
    }
    "fbuild-daemon".to_string()
}

/// HTTP client for the fbuild daemon.
pub struct DaemonClient {
    base_url: String,
    client: reqwest::Client,
}

impl DaemonClient {
    pub fn new() -> Self {
        // 100ms connect_timeout fails fast when the daemon is not running
        // (ECONNREFUSED returns instantly on Windows and Linux but reqwest
        // would otherwise wait for the full request timeout before surfacing
        // the error).
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(100))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base_url: fbuild_paths::get_daemon_url(),
            client,
        }
    }

    /// Check if the daemon is healthy.
    pub async fn health(&self) -> bool {
        self.client
            .get(format!("{}/health", self.base_url))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Get full health response including source_mtime for stale detection.
    pub async fn health_full(&self) -> Option<HealthResponseFull> {
        self.client
            .get(format!("{}/health", self.base_url))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .ok()?
            .json::<HealthResponseFull>()
            .await
            .ok()
    }

    /// List connected devices.
    pub async fn list_devices(&self, refresh: bool) -> fbuild_core::Result<DeviceListResponse> {
        let url = if refresh {
            format!("{}/api/devices/list?refresh=true", self.base_url)
        } else {
            format!("{}/api/devices/list", self.base_url)
        };
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({}))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<DeviceListResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
    }

    /// Send a build request (non-streaming, returns full response at end).
    pub async fn build(&self, req: &BuildRequest) -> fbuild_core::Result<OperationResponse> {
        // Non-streaming build is used by API/MCP callers. It can legitimately
        // wait behind another client's global package/toolchain/sidecar
        // install; do not let a client-side total timeout turn that wait into
        // a spurious failure.
        self.post_operation("/api/build", req, None).await
    }

    /// Send a streaming build request. Prints log lines in real-time,
    /// returns the final `OperationResponse` when the build completes.
    pub async fn build_streaming(
        &self,
        req: &BuildRequest,
    ) -> fbuild_core::Result<OperationResponse> {
        let resp = self
            .client
            .post(format!("{}/api/build", self.base_url))
            .json(req)
            .timeout(LONG_OPERATION_TIMEOUT)
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        // If the daemon returned a non-success status, the body is regular JSON
        // (not NDJSON). Fall back to parsing it as a standard OperationResponse.
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.map_err(|e| {
                fbuild_core::FbuildError::DaemonError(format!("read error body: {}", e))
            })?;
            if let Ok(op) = serde_json::from_str::<OperationResponse>(&body) {
                return Ok(op);
            }
            return Err(fbuild_core::FbuildError::DaemonError(format!(
                "daemon returned {} — {}",
                status, body
            )));
        }

        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut final_response: Option<OperationResponse> = None;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                fbuild_core::FbuildError::DaemonError(format!(
                    "lost connection to daemon mid-build ({}); the daemon \
                     process may have died — check {}",
                    e,
                    fbuild_paths::get_daemon_log_file().display()
                ))
            })?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete lines
            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].to_string();
                buffer = buffer[newline_pos + 1..].to_string();

                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                if let Ok(event) = serde_json::from_str::<StreamEvent>(line) {
                    match event.event_type.as_str() {
                        "log" => {
                            if let Some(msg) = event.message {
                                println!("{}", msg);
                            }
                        }
                        "status" => {
                            if let Some(msg) = stream_status_message(&event) {
                                eprintln!("{}", msg);
                            }
                        }
                        "result" => {
                            final_response = Some(OperationResponse {
                                success: event.success.unwrap_or(false),
                                request_id: event.request_id.unwrap_or_default(),
                                message: event.message.unwrap_or_default(),
                                exit_code: event.exit_code.unwrap_or(1),
                                output_file: event.output_file,
                                output_dir: event.output_dir,
                                launch_url: None,
                                stdout: None,
                                stderr: None,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }

        final_response.ok_or_else(|| {
            fbuild_core::FbuildError::DaemonError("stream ended without a result event".to_string())
        })
    }

    /// Send a deploy request.
    pub async fn deploy(&self, req: &DeployRequest) -> fbuild_core::Result<OperationResponse> {
        self.post_operation("/api/deploy", req, Some(LONG_OPERATION_TIMEOUT))
            .await
    }

    /// Send a monitor request.
    pub async fn monitor(&self, req: &MonitorRequest) -> fbuild_core::Result<OperationResponse> {
        self.post_operation("/api/monitor", req, Some(LONG_OPERATION_TIMEOUT))
            .await
    }

    /// Send a test-emu request (build + emulator run).
    pub async fn test_emu(&self, req: &TestEmuRequest) -> fbuild_core::Result<OperationResponse> {
        self.post_operation("/api/test-emu", req, Some(LONG_OPERATION_TIMEOUT))
            .await
    }

    /// Get daemon info (PID, port, uptime, etc.).
    pub async fn daemon_info(&self) -> fbuild_core::Result<DaemonInfoResponse> {
        let resp = self
            .client
            .get(format!("{}/api/daemon/info", self.base_url))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<DaemonInfoResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
    }

    /// Shut down the daemon.
    #[allow(dead_code)]
    pub async fn shutdown(&self) -> fbuild_core::Result<()> {
        let resp = self
            .client
            .post(format!("{}/api/daemon/shutdown", self.base_url))
            .headers(shutdown_caller_headers())
            .send()
            .await
            .map_err(|e| {
                fbuild_core::FbuildError::DaemonError(format!("shutdown failed: {}", e))
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(fbuild_core::FbuildError::DaemonError(format!(
                "shutdown failed with {status}: {body}"
            )));
        }
        Ok(())
    }

    /// Get lock status from the daemon.
    pub async fn lock_status(&self) -> fbuild_core::Result<LockStatusResponse> {
        let resp = self
            .client
            .get(format!("{}/api/locks/status", self.base_url))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<LockStatusResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
    }

    /// Get status for a specific device.
    pub async fn device_status(&self, port: &str) -> fbuild_core::Result<DeviceStatusResponse> {
        let resp = self
            .client
            .get(format!(
                "{}/api/devices/{}/status",
                self.base_url,
                encode_port(port)
            ))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<DeviceStatusResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
    }

    /// Acquire a lease on a device.
    pub async fn device_lease(
        &self,
        port: &str,
        lease_type: &str,
        description: &str,
    ) -> fbuild_core::Result<DeviceLeaseResponse> {
        let resp = self
            .client
            .post(format!(
                "{}/api/devices/{}/lease",
                self.base_url,
                encode_port(port)
            ))
            .json(&serde_json::json!({
                "lease_type": lease_type,
                "description": description,
            }))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<DeviceLeaseResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
    }

    /// Release a lease on a device.
    pub async fn device_release(
        &self,
        port: &str,
        lease_id: Option<&str>,
    ) -> fbuild_core::Result<DeviceReleaseResponse> {
        let body = match lease_id {
            Some(id) => serde_json::json!({"lease_id": id}),
            None => serde_json::json!({}),
        };
        let resp = self
            .client
            .post(format!(
                "{}/api/devices/{}/release",
                self.base_url,
                encode_port(port)
            ))
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<DeviceReleaseResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
    }

    /// Preempt (forcibly take) a device.
    pub async fn device_preempt(
        &self,
        port: &str,
        reason: &str,
    ) -> fbuild_core::Result<DevicePreemptResponse> {
        let resp = self
            .client
            .post(format!(
                "{}/api/devices/{}/preempt",
                self.base_url,
                encode_port(port)
            ))
            .json(&serde_json::json!({"reason": reason}))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<DevicePreemptResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
    }

    /// Clear stale locks on the daemon.
    pub async fn clear_locks(&self) -> fbuild_core::Result<ClearLocksResponse> {
        let resp = self
            .client
            .post(format!("{}/api/locks/clear", self.base_url))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<ClearLocksResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
    }

    /// Get cache statistics from the daemon.
    pub async fn cache_stats(&self) -> fbuild_core::Result<CacheStatsResponse> {
        let resp = self
            .client
            .get(format!("{}/api/cache/stats", self.base_url))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<CacheStatsResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
    }

    /// Trigger a GC run on the daemon.
    pub async fn run_gc(&self) -> fbuild_core::Result<GcResponse> {
        let resp = self
            .client
            .post(format!("{}/api/cache/gc", self.base_url))
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<GcResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
    }

    async fn post_operation<T: Serialize>(
        &self,
        path: &str,
        body: &T,
        timeout: Option<std::time::Duration>,
    ) -> fbuild_core::Result<OperationResponse> {
        let request = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(body);
        let request = match timeout {
            Some(timeout) => request.timeout(timeout),
            None => request,
        };
        let resp = request
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<OperationResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
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

/// Compute the modification time of the fbuild-daemon binary on disk.
fn compute_daemon_binary_mtime() -> f64 {
    // Find the daemon binary next to our own executable
    let daemon_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("fbuild-daemon")));

    if let Some(path) = daemon_path {
        // Try with and without .exe extension
        for candidate in [path.clone(), path.with_extension("exe")] {
            if let Ok(meta) = candidate.metadata() {
                if let Ok(mtime) = meta.modified() {
                    if let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH) {
                        return dur.as_secs_f64();
                    }
                }
            }
        }
    }
    0.0
}

/// Ensure the daemon is running. Spawn it if not.
/// If the daemon binary has been updated since the running daemon started,
/// gracefully restart it (stale source detection, matching Python behavior).
pub async fn ensure_daemon_running() -> fbuild_core::Result<()> {
    if try_acquire_broker_daemon().await? {
        return Ok(());
    }
    ensure_direct_daemon_running().await
}

async fn try_acquire_broker_daemon() -> fbuild_core::Result<bool> {
    if fbuild_paths::running_process::running_process_disabled() {
        record_daemon_acquisition(DaemonAcquisition::DisabledDirectFallback);
        return Ok(false);
    }

    let broker_endpoint = match running_process::broker::doctor::default_broker_endpoint() {
        Ok(endpoint) => endpoint,
        Err(err) => {
            record_daemon_acquisition(DaemonAcquisition::DirectFallback {
                reason: format!("could not derive broker endpoint: {err}"),
            });
            return Ok(false);
        }
    };

    let request = OwnedConnectRequest::new(
        broker_endpoint,
        fbuild_paths::running_process::SERVICE_NAME,
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_VERSION"),
    );

    match AsyncBrokerSession::adopt(request).await {
        Ok(session) => {
            let endpoint = session.endpoint().to_string();
            let daemon_version = session.negotiated().map(|n| n.daemon_version.clone());
            record_daemon_acquisition(DaemonAcquisition::BrokerNegotiated {
                endpoint,
                daemon_version,
            });

            let client = DaemonClient::new();
            for _ in 0..100 {
                if client.health().await {
                    let info = client.daemon_info().await?;
                    if let Some(err) = daemon_cache_identity_error(&info) {
                        return Err(fbuild_core::FbuildError::DaemonError(err));
                    }
                    return Ok(true);
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(fbuild_core::FbuildError::DaemonError(
                "broker negotiated fbuild-daemon, but its HTTP endpoint did not become healthy"
                    .to_string(),
            ))
        }
        Err(AdoptError::BrokerDisabled) => {
            record_daemon_acquisition(DaemonAcquisition::DisabledDirectFallback);
            Ok(false)
        }
        Err(AdoptError::DisableEnv(err)) => {
            Err(fbuild_core::FbuildError::DaemonError(err.to_string()))
        }
        Err(AdoptError::Connect(err)) => {
            if broker_refusal_is_fatal(err.refusal_kind()) {
                return Err(fbuild_core::FbuildError::DaemonError(format!(
                    "running-process broker refused fbuild daemon version: {err}"
                )));
            }
            record_daemon_acquisition(DaemonAcquisition::DirectFallback {
                reason: err.to_string(),
            });
            Ok(false)
        }
        Err(AdoptError::AsyncJoin(err)) => {
            record_daemon_acquisition(DaemonAcquisition::DirectFallback {
                reason: format!("broker adoption worker failed: {err}"),
            });
            Ok(false)
        }
    }
}

fn broker_refusal_is_fatal(kind: Option<RefusalKind>) -> bool {
    matches!(
        kind,
        Some(RefusalKind::VersionUnsupported | RefusalKind::VersionBlocked)
    )
}

/// Legacy direct HTTP daemon acquisition path.
async fn ensure_direct_daemon_running() -> fbuild_core::Result<()> {
    let client = DaemonClient::new();

    // Check if already running
    if client.health().await {
        // Check if daemon binary is stale (updated since daemon started)
        if let Some(health) = client.health_full().await {
            if health.source_mtime > 0.0 {
                let current_mtime = compute_daemon_binary_mtime();
                if current_mtime > 0.0 && current_mtime > health.source_mtime {
                    tracing::info!(
                        "daemon binary is stale (daemon={}, current={}), restarting...",
                        health.source_mtime,
                        current_mtime
                    );
                    eprintln!("daemon binary updated, restarting...");
                    let _ = client.shutdown().await;
                    // Wait for it to stop
                    for _ in 0..50 {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        if !client.health().await {
                            break;
                        }
                    }
                    // Fall through to spawn a fresh daemon below
                } else {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        } else {
            return Ok(());
        }
    }

    tracing::info!("daemon not running, starting...");

    // Retry daemon spawn up to 3 times with exponential backoff
    // (matches Python behavior: [0.0s, 0.5s, 2.0s] delays between attempts)
    let backoff_delays = [0.0, 0.5, 2.0];

    for (attempt, &delay) in backoff_delays.iter().enumerate() {
        if attempt > 0 {
            tracing::info!(
                "spawn attempt {}/{} (backoff {:.1}s)",
                attempt + 1,
                backoff_delays.len(),
                delay
            );
            tokio::time::sleep(std::time::Duration::from_secs_f64(delay)).await;
        }

        if let Err(e) = spawn_daemon_process().await {
            tracing::warn!("daemon spawn attempt {} failed: {}", attempt + 1, e);
            if attempt + 1 >= backoff_delays.len() {
                return Err(e);
            }
            continue;
        }

        // Poll health for up to 10 seconds
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if client.health().await {
                tracing::info!("daemon started successfully");
                return Ok(());
            }
        }

        tracing::warn!(
            "daemon did not become healthy after spawn attempt {}",
            attempt + 1
        );
    }

    Err(fbuild_core::FbuildError::DaemonError(
        "daemon did not start after 3 attempts".to_string(),
    ))
}

/// Spawn a single daemon process instance.
async fn spawn_daemon_process() -> fbuild_core::Result<()> {
    let daemon_exe = "fbuild-daemon";
    // allow-direct-spawn: daemon must outlive the CLI; see INTENTIONALLY DETACHED comment below.
    let mut cmd = tokio::process::Command::new(daemon_exe);
    tracing::debug!(
        "running-process broker adoption status: {}",
        fbuild_paths::running_process::running_process_adoption_summary()
    );

    if fbuild_paths::is_dev_mode() {
        cmd.arg("--dev");
    }

    // Pass the spawner's working directory so the daemon can track who spawned it
    if let Ok(cwd) = std::env::current_dir() {
        cmd.arg(format!("--spawner-cwd={}", cwd.display()));
    }

    // Propagate VIRTUAL_ENV so the daemon can find zccache from .venv
    if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
        cmd.env("VIRTUAL_ENV", venv);
    }

    // Prevent a console window from appearing on Windows (including MSYS/MinGW).
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const DETACHED_PROCESS: u32 = 0x00000008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }

    // Redirect stderr to log file so daemon logs are persisted
    let daemon_dir = fbuild_paths::get_daemon_dir();
    let _ = std::fs::create_dir_all(&daemon_dir);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(fbuild_paths::get_daemon_log_file())
        .map_err(|e| {
            fbuild_core::FbuildError::DaemonError(format!("failed to open log file: {}", e))
        })?;

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(log_file));

    // On Windows, any inheritable handle in our process — including the shell's
    // stderr pipe — flows into the daemon grandchild via bInheritHandles=TRUE
    // in CreateProcess. The daemon holds that pipe open for SELF_EVICTION_TIMEOUT
    // (120s), blocking the shell from unblocking even after the CLI exits. Strip
    // HANDLE_FLAG_INHERIT from our std handles before spawn so Rust's plumbing
    // only passes through the explicit Stdio handles it configured above.
    // See issue #91.
    #[cfg(windows)]
    strip_std_handle_inheritance();

    // INTENTIONALLY DETACHED (FastLED/fbuild#32): the CLI spawns the
    // daemon and then exits — the daemon must survive the CLI. The
    // daemon in turn installs its own global `ContainedProcessGroup`
    // (see fbuild-daemon/src/main.rs) so every descendant it spawns
    // dies with *it*. The CLI binary itself has no global containment
    // group installed, so this `spawn()` is already uncontained; the
    // comment is here so a future refactor doesn't accidentally reroute
    // it through `containment::spawn_contained`, which would make the
    // daemon die the instant the CLI exits.
    cmd.spawn().map_err(|e| {
        fbuild_core::FbuildError::DaemonError(format!(
            "failed to spawn daemon (is fbuild-daemon in PATH?): {}",
            e
        ))
    })?;

    Ok(())
}

/// Clear `HANDLE_FLAG_INHERIT` on the CLI's STD_INPUT/OUTPUT/ERROR handles.
///
/// Called immediately before spawning the daemon so the parent shell's pipes
/// (attached to our stderr via `|`, `2>&1`, etc.) are not inherited by the
/// long-lived daemon grandchild.
#[cfg(windows)]
fn strip_std_handle_inheritance() {
    use std::ffi::c_void;

    const HANDLE_FLAG_INHERIT: u32 = 0x1;
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const INVALID_HANDLE_VALUE: isize = -1;

    unsafe extern "system" {
        fn GetStdHandle(nStdHandle: u32) -> *mut c_void;
        fn SetHandleInformation(hObject: *mut c_void, dwMask: u32, dwFlags: u32) -> i32;
    }

    for std_id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle / SetHandleInformation are documented safe for
        // concurrent use on owned std handles. We only clear a flag on handles
        // the OS already owns on our behalf; we do not close them.
        unsafe {
            let h = GetStdHandle(std_id);
            if !h.is_null() && (h as isize) != INVALID_HANDLE_VALUE {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        broker_refusal_is_fatal, daemon_cache_identity_error, DaemonAcquisition, DaemonInfoResponse,
    };
    use running_process::broker::client::RefusalKind::{VersionBlocked, VersionUnsupported};

    #[test]
    fn broker_version_refusals_are_fatal() {
        assert!(broker_refusal_is_fatal(Some(VersionUnsupported)));
        assert!(broker_refusal_is_fatal(Some(VersionBlocked)));
    }

    #[test]
    fn broker_non_refusal_errors_can_fallback() {
        assert!(!broker_refusal_is_fatal(None));
    }

    #[test]
    fn broker_acquisition_reports_negotiated_state() {
        let acquisition = DaemonAcquisition::BrokerNegotiated {
            endpoint: "rp-backend".to_string(),
            daemon_version: Some("2.2.29".to_string()),
        };

        assert_eq!(acquisition.mode(), "broker-negotiated");
        assert_eq!(acquisition.endpoint(), Some("rp-backend"));
        assert_eq!(acquisition.daemon_version(), Some("2.2.29"));
        assert_eq!(acquisition.reason(), None);
        assert!(acquisition.summary().contains("version 2.2.29"));
    }

    #[test]
    fn direct_acquisition_reports_fallback_reason() {
        let acquisition = DaemonAcquisition::DirectFallback {
            reason: "broker unavailable".to_string(),
        };

        assert_eq!(acquisition.mode(), "direct-fallback");
        assert_eq!(acquisition.endpoint(), None);
        assert_eq!(acquisition.daemon_version(), None);
        assert_eq!(acquisition.reason(), Some("broker unavailable"));
        assert!(acquisition.summary().contains("broker unavailable"));
    }

    fn daemon_info_for_cache_identity(
        cache_identity: Option<String>,
        cache_schema_version: Option<u32>,
    ) -> DaemonInfoResponse {
        DaemonInfoResponse {
            status: "running".to_string(),
            uptime_seconds: 1.0,
            version: "2.2.29".to_string(),
            pid: 123,
            port: 8765,
            dev_mode: fbuild_paths::is_dev_mode(),
            operation_in_progress: false,
            daemon_state: fbuild_core::DaemonState::Idle,
            current_operation: None,
            dependency_install: None,
            client_count: 0,
            cache_identity,
            cache_schema_version,
            spawner_cwd: None,
            source_mtime: None,
        }
    }

    #[test]
    fn daemon_cache_identity_accepts_current_identity() {
        let identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
        let info = daemon_info_for_cache_identity(
            Some(identity.label_value()),
            Some(fbuild_paths::running_process::CACHE_SCHEMA_VERSION),
        );

        assert!(daemon_cache_identity_error(&info).is_none());
    }

    #[test]
    fn daemon_cache_identity_rejects_missing_identity() {
        let info = daemon_info_for_cache_identity(
            None,
            Some(fbuild_paths::running_process::CACHE_SCHEMA_VERSION),
        );

        let err = daemon_cache_identity_error(&info).expect("missing identity must fail closed");
        assert!(err.contains("cache identity"));
    }

    #[test]
    fn daemon_cache_identity_rejects_wrong_schema() {
        let identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
        let info = daemon_info_for_cache_identity(Some(identity.label_value()), Some(u32::MAX));

        let err = daemon_cache_identity_error(&info).expect("schema mismatch must fail closed");
        assert!(err.contains("cache schema"));
    }
}
