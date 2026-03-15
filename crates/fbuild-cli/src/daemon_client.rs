//! HTTP client for communicating with the fbuild daemon.

use serde::{Deserialize, Serialize};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OperationResponse {
    pub success: bool,
    #[allow(dead_code)]
    pub request_id: String,
    pub message: String,
    pub exit_code: i32,
}

/// HTTP client for the fbuild daemon.
pub struct DaemonClient {
    base_url: String,
    client: reqwest::Client,
}

impl DaemonClient {
    pub fn new() -> Self {
        Self {
            base_url: fbuild_paths::get_daemon_url(),
            client: reqwest::Client::new(),
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

    /// Send a build request.
    pub async fn build(&self, req: &BuildRequest) -> fbuild_core::Result<OperationResponse> {
        self.post("/api/build", req).await
    }

    /// Send a deploy request.
    pub async fn deploy(&self, req: &DeployRequest) -> fbuild_core::Result<OperationResponse> {
        self.post("/api/deploy", req).await
    }

    /// Send a monitor request.
    pub async fn monitor(&self, req: &MonitorRequest) -> fbuild_core::Result<OperationResponse> {
        self.post("/api/monitor", req).await
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

/// Ensure the daemon is running. Spawn it if not.
pub async fn ensure_daemon_running() -> fbuild_core::Result<()> {
    let client = DaemonClient::new();

    // Check if already running
    if client.health().await {
        return Ok(());
    }

    tracing::info!("daemon not running, starting...");

    // Spawn the daemon as a detached process
    let daemon_exe = "fbuild-daemon";
    let mut cmd = tokio::process::Command::new(daemon_exe);

    if fbuild_paths::is_dev_mode() {
        cmd.arg("--dev");
    }

    // On native Windows (not MSYS), use CREATE_NO_WINDOW + DETACHED_PROCESS
    // to prevent a console window from appearing.
    #[allow(unused_imports, unreachable_code, unused_variables)]
    #[cfg(all(target_os = "windows", not(target_env = "gnu")))]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const DETACHED_PROCESS: u32 = 0x00000008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    cmd.spawn().map_err(|e| {
        fbuild_core::FbuildError::DaemonError(format!(
            "failed to spawn daemon (is fbuild-daemon in PATH?): {}",
            e
        ))
    })?;

    // Poll health for up to 10 seconds
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if client.health().await {
            tracing::info!("daemon started successfully");
            return Ok(());
        }
    }

    Err(fbuild_core::FbuildError::DaemonError(
        "daemon did not start within 10 seconds".to_string(),
    ))
}
