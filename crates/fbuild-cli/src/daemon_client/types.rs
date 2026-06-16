use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
    /// Override for PLATFORMIO_SRC_DIR - forwarded from caller's environment.
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
    /// Override for PLATFORMIO_SRC_DIR - forwarded from caller's environment.
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
pub(super) struct StreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub message: Option<String>,
    pub success: Option<bool>,
    pub request_id: Option<String>,
    pub exit_code: Option<i32>,
    pub output_file: Option<String>,
    pub output_dir: Option<String>,
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
    pub dependency_install: Option<fbuild_core::install_status::InstallStatus>,
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
