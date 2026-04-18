//! HTTP client for communicating with the fbuild daemon.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Percent-encode a port name for use in a URL path segment.
/// Linux ports like `/dev/ttyUSB0` contain slashes that break URL routing.
fn encode_port(port: &str) -> String {
    port.replace('%', "%25").replace('/', "%2F")
}

/// Request/response types (defined locally, no dependency on fbuild-daemon binary crate).

#[derive(Debug, Serialize)]
pub struct BuildRequest {
    pub project_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub clean_build: bool,
    pub verbose: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jobs: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default)]
    pub generate_compiledb: bool,
    #[serde(default)]
    pub compiledb_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_cwd: Option<String>,
    /// When true, request a streaming NDJSON response.
    #[serde(default)]
    pub stream: bool,
    /// When true, run symbol-level memory analysis after linking.
    #[serde(default)]
    pub symbol_analysis: bool,
    /// Optional path to write the symbol analysis report to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_analysis_path: Option<String>,
    /// Disable elapsed-time prefix on build output lines.
    #[serde(default)]
    pub no_timestamp: bool,
    /// Override for PLATFORMIO_SRC_DIR — forwarded from caller's environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_dir: Option<String>,
    /// Export a tooling-friendly artifact bundle to this directory after build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    /// Snapshot of all `PLATFORMIO_*` env vars from the caller's environment.
    /// The daemon does not inherit caller env vars, so they are forwarded here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pio_env: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct DeployRequest {
    pub project_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    pub monitor_after: bool,
    pub skip_build: bool,
    pub clean_build: bool,
    pub verbose: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_timeout: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_halt_on_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_halt_on_success: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_expect: Option<String>,
    pub monitor_show_timestamp: bool,
    /// Override the board's default upload baud rate for flashing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baud_rate: Option<u32>,
    /// Deploy destination: "device", "emu", or "emulator".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Emulator backend when deploying to `emu`/`emulator`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emulator: Option<String>,
    /// Legacy deploy target alias: "device", "qemu", or "avr8js".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default)]
    pub qemu: bool,
    #[serde(default)]
    pub qemu_timeout: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_cwd: Option<String>,
    /// Override for PLATFORMIO_SRC_DIR — forwarded from caller's environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_dir: Option<String>,
    /// Export a tooling-friendly artifact bundle to this directory after build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    /// Snapshot of all `PLATFORMIO_*` env vars from the caller's environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pio_env: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct MonitorRequest {
    pub project_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baud_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halt_on_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halt_on_success: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    pub show_timestamp: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_cwd: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TestEmuRequest {
    pub project_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub verbose: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halt_on_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halt_on_success: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emulator: Option<String>,
    pub show_timestamp: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pio_env: BTreeMap<String, String>,
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

#[derive(Debug, Deserialize)]
pub struct OperationResponse {
    pub success: bool,
    #[allow(dead_code)]
    pub request_id: String,
    pub message: String,
    pub exit_code: i32,
    #[allow(dead_code)]
    pub output_file: Option<String>,
    #[allow(dead_code)]
    pub output_dir: Option<String>,
    #[allow(dead_code)]
    pub launch_url: Option<String>,
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
}

/// NDJSON event from a streaming build response.
#[derive(Debug, Deserialize)]
struct StreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    message: Option<String>,
    success: Option<bool>,
    request_id: Option<String>,
    exit_code: Option<i32>,
    output_file: Option<String>,
    output_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DaemonInfoResponse {
    #[allow(dead_code)]
    pub status: String,
    pub uptime_seconds: f64,
    pub version: String,
    pub pid: u32,
    pub port: u16,
    pub dev_mode: bool,
    #[serde(default)]
    pub operation_in_progress: bool,
    #[serde(default)]
    pub daemon_state: fbuild_core::DaemonState,
    pub current_operation: Option<String>,
    #[serde(default)]
    pub client_count: usize,
    #[serde(default)]
    pub spawner_cwd: Option<String>,
    #[serde(default)]
    pub source_mtime: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct LockStatusResponse {
    #[allow(dead_code)]
    pub success: bool,
    pub port_locks: Vec<PortLockInfo>,
    pub project_locks: Vec<ProjectLockInfo>,
    pub stale_locks: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PortLockInfo {
    pub port: String,
    pub is_held: bool,
    #[allow(dead_code)]
    pub holder_description: Option<String>,
    pub is_open: bool,
    pub writer_client_id: Option<String>,
    pub reader_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct ProjectLockInfo {
    pub project_dir: String,
    pub is_held: bool,
}

#[derive(Debug, Deserialize)]
pub struct ClearLocksResponse {
    #[allow(dead_code)]
    pub success: bool,
    pub cleared_count: usize,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct CacheStatsResponse {
    pub success: bool,
    pub archive_bytes: u64,
    pub installed_bytes: u64,
    pub total_bytes: u64,
    pub entry_count: i64,
    pub high_watermark: u64,
    pub low_watermark: u64,
    pub archive_budget: u64,
    #[serde(default)]
    #[allow(dead_code)]
    pub installed_budget: u64,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GcResponse {
    pub success: bool,
    pub installed_evicted: u64,
    pub installed_bytes_freed: u64,
    pub archives_evicted: u64,
    pub archive_bytes_freed: u64,
    pub total_bytes_freed: u64,
    #[serde(default)]
    pub orphan_files_removed: usize,
    #[serde(default)]
    pub orphan_rows_cleaned: usize,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HealthResponseFull {
    #[allow(dead_code)]
    pub status: String,
    #[allow(dead_code)]
    pub uptime_seconds: f64,
    #[allow(dead_code)]
    pub version: String,
    #[allow(dead_code)]
    pub pid: u32,
    #[serde(default)]
    pub source_mtime: f64,
}

#[derive(Debug, Deserialize)]
pub struct DeviceListResponse {
    #[allow(dead_code)]
    pub success: bool,
    pub devices: Vec<DeviceInfoResponse>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct DeviceInfoResponse {
    pub port: String,
    pub device_id: Option<String>,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct DeviceStatusResponse {
    pub success: bool,
    pub port: String,
    pub device_id: String,
    pub description: String,
    pub is_connected: bool,
    pub available_for_exclusive: bool,
    pub exclusive_holder: Option<String>,
    pub monitor_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct DeviceLeaseResponse {
    pub success: bool,
    pub lease_id: Option<String>,
    #[allow(dead_code)]
    pub lease_type: Option<String>,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct DeviceReleaseResponse {
    pub success: bool,
    #[allow(dead_code)]
    pub released_count: usize,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct DevicePreemptResponse {
    pub success: bool,
    #[allow(dead_code)]
    pub lease_id: Option<String>,
    pub message: String,
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
        self.post("/api/build", req).await
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
            .timeout(std::time::Duration::from_secs(1800))
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
                fbuild_core::FbuildError::DaemonError(format!("stream error: {}", e))
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
        self.post("/api/deploy", req).await
    }

    /// Send a monitor request.
    pub async fn monitor(&self, req: &MonitorRequest) -> fbuild_core::Result<OperationResponse> {
        self.post("/api/monitor", req).await
    }

    /// Send a test-emu request (build + emulator run).
    pub async fn test_emu(&self, req: &TestEmuRequest) -> fbuild_core::Result<OperationResponse> {
        self.post("/api/test-emu", req).await
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
        self.client
            .post(format!("{}/api/daemon/shutdown", self.base_url))
            .send()
            .await
            .map_err(|e| {
                fbuild_core::FbuildError::DaemonError(format!("shutdown failed: {}", e))
            })?;
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

    async fn post<T: Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> fbuild_core::Result<OperationResponse> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(body)
            .timeout(std::time::Duration::from_secs(1800))
            .send()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("request failed: {}", e)))?;

        resp.json::<OperationResponse>()
            .await
            .map_err(|e| fbuild_core::FbuildError::DaemonError(format!("invalid response: {}", e)))
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
    let mut cmd = tokio::process::Command::new(daemon_exe);

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
