use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct BuildRequest {
    pub project_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub clean_build: bool,
    #[serde(default)]
    pub clean_all: bool,
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
    /// When true, append `-Wl,--noinhibit-exec` to the linker command and
    /// treat post-link "failure" as success when `firmware.elf` was emitted
    /// (so over-budget builds can still be bloat-analyzed).
    /// See FastLED/fbuild#594.
    #[serde(default)]
    pub bloat_analysis: bool,
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
    #[serde(default)]
    pub clean_all: bool,
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
    /// Force LPC deploys through lpc21isp instead of the probe-rs SWD fast path.
    #[serde(default)]
    pub no_probe_rs: bool,
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
    pub current_operation: Option<String>,
    pub operation_in_progress: Option<bool>,
    pub dependency_install: Option<fbuild_core::install_status::InstallStatus>,
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
    pub cache_identity: Option<String>,
    #[serde(default)]
    pub cache_schema_version: Option<u32>,
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
    #[serde(default)]
    pub pending_serial_attaches: Vec<PendingSerialAttachInfo>,
    pub stale_locks: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PortLockInfo {
    pub port: String,
    pub is_held: bool,
    #[allow(dead_code)]
    pub holder_description: Option<String>,
    pub is_open: bool,
    #[serde(default)]
    pub owner_client_id: Option<String>,
    pub writer_client_id: Option<String>,
    pub reader_count: usize,
    #[serde(default)]
    #[allow(dead_code)]
    pub reader_client_ids: Vec<String>,
    #[serde(default)]
    pub baud_rate: u32,
    #[serde(default)]
    #[allow(dead_code)]
    pub started_at: f64,
    #[serde(default)]
    pub session_age_seconds: f64,
    #[serde(default)]
    #[allow(dead_code)]
    pub last_activity_at: f64,
    #[serde(default)]
    pub last_activity_age_seconds: f64,
    #[serde(default)]
    #[allow(dead_code)]
    pub last_read_at: Option<f64>,
    #[serde(default)]
    pub last_read_age_seconds: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)]
    pub last_write_at: Option<f64>,
    #[serde(default)]
    pub last_write_age_seconds: Option<f64>,
    #[serde(default)]
    pub total_bytes_read: u64,
    #[serde(default)]
    pub total_bytes_written: u64,
    #[serde(default)]
    pub clients: Vec<SerialClientLockInfo>,
}

#[derive(Debug, Deserialize)]
pub struct SerialClientLockInfo {
    pub client_id: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub process_alive: Option<bool>,
    #[serde(default)]
    pub exe: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub argv: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct PendingSerialAttachInfo {
    pub id: u64,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub port: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub started_at: f64,
    #[serde(default)]
    pub age_seconds: f64,
}

#[derive(Debug, Deserialize)]
pub struct ProjectLockInfo {
    pub project_dir: String,
    pub is_held: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct ClearLocksRequest {
    #[serde(default)]
    pub serial: bool,
    #[serde(default)]
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize)]
pub struct ClearLocksResponse {
    #[allow(dead_code)]
    pub success: bool,
    pub cleared_count: usize,
    #[serde(default)]
    pub cleared_project_count: usize,
    #[serde(default)]
    pub cleared_serial_count: usize,
    #[serde(default)]
    pub cleared_serial_sessions: Vec<String>,
    #[serde(default)]
    pub refused: Vec<String>,
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
    /// Pretty USB vendor name resolved by the daemon from FastLED/boards.
    /// `None` for non-USB ports.
    #[serde(default)]
    pub vendor_name: Option<String>,
    /// Pretty USB product name (same provenance as `vendor_name`).
    #[serde(default)]
    pub product_name: Option<String>,
    /// `true` for CDC-ACM, `false` for a USB-serial bridge, `None` for
    /// unknown or older daemons that did not report the field.
    #[serde(default)]
    pub is_cdc: Option<bool>,
    #[serde(default)]
    pub serial_number: Option<String>,
    #[serde(default)]
    pub previous_port: Option<String>,
    pub description: String,
    #[serde(default)]
    pub available_for_exclusive: bool,
    #[serde(default)]
    pub exclusive_lease: Option<DeviceLeaseInfoResponse>,
    #[serde(default)]
    pub monitor_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct DeviceStatusResponse {
    pub success: bool,
    pub port: String,
    pub device_id: String,
    pub description: String,
    #[serde(default)]
    pub vid: Option<u16>,
    #[serde(default)]
    pub pid: Option<u16>,
    /// Pretty USB vendor name resolved by the daemon. `None` for
    /// bluetooth/PCI/unknown serials.
    #[serde(default)]
    pub vendor_name: Option<String>,
    /// Pretty USB product name (same provenance as `vendor_name`).
    #[serde(default)]
    pub product_name: Option<String>,
    /// `true` for CDC-ACM, `false` for a USB-serial bridge, `None` for
    /// unknown or older daemons that did not report the field.
    #[serde(default)]
    pub is_cdc: Option<bool>,
    #[serde(default)]
    pub serial_number: Option<String>,
    #[serde(default)]
    pub previous_port: Option<String>,
    pub is_connected: bool,
    pub available_for_exclusive: bool,
    pub exclusive_holder: Option<String>,
    #[serde(default)]
    pub exclusive_lease: Option<DeviceLeaseInfoResponse>,
    pub monitor_count: usize,
    #[serde(default)]
    pub monitor_leases: Vec<DeviceLeaseInfoResponse>,
}

#[derive(Debug, Deserialize)]
pub struct DeviceLeaseResponse {
    pub success: bool,
    pub lease_id: Option<String>,
    #[allow(dead_code)]
    pub lease_type: Option<String>,
    pub message: String,
    #[serde(default)]
    pub conflict: Option<DeviceLeaseConflictResponse>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceLeaseInfoResponse {
    pub lease_id: String,
    pub client_id: String,
    pub lease_type: String,
    pub description: String,
    pub acquired_at: f64,
    #[serde(default)]
    pub track_serial: bool,
}

#[derive(Debug, Deserialize)]
pub struct DeviceLeaseConflictResponse {
    pub port: String,
    pub device_id: String,
    pub description: String,
    pub holder: DeviceLeaseInfoResponse,
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
