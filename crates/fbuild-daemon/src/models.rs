//! Request/response JSON types matching the Python daemon's API contract.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::lock_models::{
    ClearLocksRequest, ClearLocksResponse, LockStatusResponse, PendingSerialAttachLockInfo,
    PortLockInfo, ProjectLockInfo, SerialClientLockInfo,
};

/// POST /api/build
#[derive(Debug, Deserialize)]
pub struct BuildRequest {
    pub project_dir: String,
    pub environment: Option<String>,
    #[serde(default, alias = "clean")]
    pub clean_build: bool,
    /// Remove matching reusable framework caches as well as project output.
    #[serde(default)]
    pub clean_all: bool,
    /// Remove outputs/cache entries without compiling or linking.
    #[serde(default)]
    pub clean_only: bool,
    #[serde(default)]
    pub verbose: bool,
    pub jobs: Option<usize>,
    pub profile: Option<String>,
    #[serde(default)]
    pub generate_compiledb: bool,
    /// Skip compilation/linking and only generate `compile_commands.json`.
    #[serde(default)]
    pub compiledb_only: bool,
    pub request_id: Option<String>,
    /// PID of the CLI client that sent this request (audit/tracking).
    pub caller_pid: Option<u32>,
    /// Working directory of the CLI client (audit/tracking).
    pub caller_cwd: Option<String>,
    /// When true, return a streaming NDJSON response instead of a single JSON object.
    #[serde(default)]
    pub stream: bool,
    /// When true, run symbol-level memory analysis after linking.
    #[serde(default)]
    pub symbol_analysis: bool,
    /// Optional path to write the symbol analysis report to.
    pub symbol_analysis_path: Option<String>,
    /// Disable elapsed-time prefix on build output lines.
    #[serde(default)]
    pub no_timestamp: bool,
    /// Override for PLATFORMIO_SRC_DIR — the source directory to compile.
    /// Forwarded from the CLI caller's environment since the daemon process
    /// does not inherit the caller's env vars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src_dir: Option<String>,
    /// Export a tooling-friendly artifact bundle to this directory after build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    /// Snapshot of `PLATFORMIO_*` env vars from the CLI caller's environment.
    ///
    /// The daemon does not inherit caller env vars, so the CLI forwards them
    /// here per request. Consumed by `fbuild-config::PioEnvOverrides`.
    #[serde(default)]
    pub pio_env: BTreeMap<String, String>,
    /// Optional explicit build-dir root override. Takes precedence over
    /// `FBUILD_BUILD_DIR` and the default `<project>/.fbuild/build`.
    /// `<env>/<profile>` is still appended (unless `flatten_env` is set).
    /// See FastLED/fbuild#432.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_dir_override: Option<String>,
    /// When true, drop the `<env>` segment of the build-dir path.
    /// Useful when the project directory is already named after the env
    /// (e.g. FastLED's `.build/pio/<board>/`). The auto-collapse rule
    /// in [`fbuild_paths::BuildLayout`] does this automatically when the
    /// project basename equals the env name; this flag lets callers force
    /// the same behavior in other shapes.
    #[serde(default)]
    pub flatten_env: bool,
    /// When true, append `-Wl,--noinhibit-exec` to the linker command and
    /// treat post-link "failure" as success when `firmware.elf` was emitted
    /// (so over-budget builds can still be bloat-analyzed).
    /// See FastLED/fbuild#594.
    #[serde(default)]
    pub bloat_analysis: bool,
}

/// POST /api/deploy
#[derive(Debug, Deserialize)]
pub struct DeployRequest {
    pub project_dir: String,
    pub environment: Option<String>,
    pub port: Option<String>,
    /// Explicit deploy protocol, currently `isp` or `wlink` for CH32V.
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub monitor_after: bool,
    #[serde(default)]
    pub skip_build: bool,
    #[serde(default, alias = "clean")]
    pub clean_build: bool,
    /// Remove matching reusable framework caches as well as project output.
    #[serde(default)]
    pub clean_all: bool,
    #[serde(default)]
    pub verbose: bool,
    pub monitor_timeout: Option<f64>,
    pub monitor_halt_on_error: Option<String>,
    pub monitor_halt_on_success: Option<String>,
    pub monitor_expect: Option<String>,
    #[serde(default = "default_true")]
    pub monitor_show_timestamp: bool,
    /// Override the board's default upload baud rate for flashing.
    pub baud_rate: Option<u32>,
    /// Force LPC deploys through lpc21isp instead of the probe-rs SWD fast path.
    #[serde(default)]
    pub no_probe_rs: bool,
    /// Deploy destination: "device", "emu", or "emulator".
    pub to: Option<String>,
    /// Emulator backend when deploying to `emu`/`emulator`.
    pub emulator: Option<String>,
    /// Legacy deploy target alias: "device" (default), "qemu", or "avr8js".
    pub target: Option<String>,
    #[serde(default)]
    pub qemu: bool,
    #[serde(default = "default_qemu_timeout")]
    pub qemu_timeout: u32,
    pub request_id: Option<String>,
    pub caller_pid: Option<u32>,
    pub caller_cwd: Option<String>,
    /// Override for PLATFORMIO_SRC_DIR — the source directory to compile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src_dir: Option<String>,
    /// Export a tooling-friendly artifact bundle to this directory after build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    /// Snapshot of `PLATFORMIO_*` env vars from the CLI caller's environment.
    #[serde(default)]
    pub pio_env: BTreeMap<String, String>,
    /// Optional explicit build-dir root override (see [`BuildRequest::build_dir_override`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_dir_override: Option<String>,
    /// Drop the `<env>` segment of the build-dir path (see [`BuildRequest::flatten_env`]).
    #[serde(default)]
    pub flatten_env: bool,
    /// User permission for the CLI-side one-shot USB recovery helper. The
    /// daemon remains unprivileged and merely carries this typed policy.
    #[serde(default)]
    pub usb_recovery_policy: fbuild_core::usb::UsbRecoveryPolicy,
}

fn default_qemu_timeout() -> u32 {
    30
}

/// POST /api/monitor
#[derive(Debug, Deserialize)]
pub struct MonitorRequest {
    pub project_dir: String,
    pub environment: Option<String>,
    pub port: Option<String>,
    pub baud_rate: Option<u32>,
    pub halt_on_error: Option<String>,
    pub halt_on_success: Option<String>,
    pub expect: Option<String>,
    pub timeout: Option<f64>,
    #[serde(default = "default_true")]
    pub show_timestamp: bool,
    pub request_id: Option<String>,
    pub caller_pid: Option<u32>,
    pub caller_cwd: Option<String>,
    /// Attempt one ESP DTR/RTS hard-reset when ROM download-mode is
    /// detected mid-monitor, instead of fast-failing with the diagnostic.
    /// Default `false` preserves the pre-#577 behaviour. See
    /// FastLED/fbuild#532.
    #[serde(default)]
    pub auto_recover_from_download_mode: bool,
}

fn default_true() -> bool {
    true
}

/// Generic operation response.
#[derive(Debug, Serialize)]
pub struct OperationResponse {
    pub success: bool,
    pub request_id: String,
    pub message: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_url: Option<String>,
    /// Captured stdout from the deploy/build tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    /// Captured stderr from the deploy/build tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

impl OperationResponse {
    pub fn ok(request_id: String, message: String) -> Self {
        Self {
            success: true,
            request_id,
            message,
            exit_code: 0,
            output_file: None,
            output_dir: None,
            launch_url: None,
            stdout: None,
            stderr: None,
        }
    }

    pub fn fail(request_id: String, message: String) -> Self {
        Self {
            success: false,
            request_id,
            message,
            exit_code: 1,
            output_file: None,
            output_dir: None,
            launch_url: None,
            stdout: None,
            stderr: None,
        }
    }
}

/// GET /health
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub uptime_seconds: f64,
    pub version: String,
    pub pid: u32,
    pub source_mtime: f64,
}

/// GET /api/daemon/info
#[derive(Debug, Serialize)]
pub struct DaemonInfoResponse {
    pub status: String,
    pub uptime_seconds: f64,
    pub version: String,
    pub pid: u32,
    pub port: u16,
    pub started_at: f64,
    pub dev_mode: bool,
    pub host: String,
    pub operation_in_progress: bool,
    pub daemon_state: fbuild_core::DaemonState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_install: Option<fbuild_core::install_status::InstallStatus>,
    pub client_count: usize,
    pub cache_dir: String,
    pub cache_identity: String,
    pub cache_schema_version: u32,
    pub daemon_dir: String,
    pub source_mtime: f64,
    pub spawner_cwd: String,
    /// MCP (Model Context Protocol) server URL.
    pub mcp_url: String,
    /// Watch-set fingerprint cache counters (#123). Surfaced on
    /// `/api/daemon/info` so operators can validate the cache is
    /// serving hits in the field without scraping tracing logs.
    /// Skipped on older daemons that predate the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch_set_cache: Option<crate::watch_set_cache::WatchSetCacheStats>,
}

/// GET / (root endpoint)
#[derive(Debug, Serialize)]
pub struct RootResponse {
    pub message: String,
    pub version: String,
    pub health: String,
}

/// POST /api/daemon/shutdown
#[derive(Debug, Serialize)]
pub struct ShutdownResponse {
    pub message: String,
}

/// Query params for shutdown endpoint.
#[derive(Debug, Deserialize)]
pub struct ShutdownParams {
    pub force: Option<bool>,
}

/// Device information returned by device list.
#[derive(Debug, Serialize)]
pub struct DeviceInfo {
    pub port: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u16>,
    /// Human-readable USB vendor name resolved via `fbuild_core::usb`. Only
    /// emitted when the device has a USB VID (bluetooth/PCI/unknown serials
    /// omit this).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_name: Option<String>,
    /// Human-readable USB product name (same provenance as `vendor_name`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_name: Option<String>,
    /// `true` for CDC-ACM, `false` for a USB-serial bridge, `null` if
    /// the host could not classify this port.
    pub is_cdc: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_port: Option<String>,
    pub description: String,
    pub available_for_exclusive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_lease: Option<DeviceLeaseInfo>,
    pub monitor_count: usize,
}

/// POST /api/devices/list response.
#[derive(Debug, Serialize)]
pub struct DeviceListResponse {
    pub success: bool,
    pub devices: Vec<DeviceInfo>,
}

/// POST /api/install-deps request.
#[derive(Debug, Deserialize)]
pub struct InstallDepsRequest {
    pub project_dir: String,
    pub environment: Option<String>,
    pub request_id: Option<String>,
    pub caller_pid: Option<u32>,
    pub caller_cwd: Option<String>,
}

/// POST /api/devices/{port}/lease request.
#[derive(Debug, Deserialize)]
pub struct DeviceLeaseRequest {
    /// "exclusive" or "monitor"
    #[serde(default = "default_exclusive")]
    pub lease_type: String,
    /// Description of why the lease is needed
    #[serde(default)]
    pub description: String,
    /// Client identifier (auto-generated if omitted)
    pub client_id: Option<String>,
    /// Follow a USB device by serial number across port renumbering.
    #[serde(default)]
    pub track_serial: bool,
}

fn default_exclusive() -> String {
    "exclusive".to_string()
}

/// POST /api/devices/{port}/lease response.
#[derive(Debug, Serialize)]
pub struct DeviceLeaseResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_type: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<DeviceLeaseConflict>,
}

/// Public lease attribution record.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceLeaseInfo {
    pub lease_id: String,
    pub client_id: String,
    pub lease_type: String,
    pub description: String,
    pub acquired_at: f64,
    pub track_serial: bool,
}

/// Structured details for an exclusive lease conflict.
#[derive(Debug, Serialize)]
pub struct DeviceLeaseConflict {
    pub port: String,
    pub device_id: String,
    pub description: String,
    pub holder: DeviceLeaseInfo,
}

/// POST /api/devices/{port}/release request.
#[derive(Debug, Deserialize)]
pub struct DeviceReleaseRequest {
    /// Specific lease ID to release. If omitted, releases all leases on the device.
    pub lease_id: Option<String>,
}

/// POST /api/devices/{port}/release response.
#[derive(Debug, Serialize)]
pub struct DeviceReleaseResponse {
    pub success: bool,
    pub released_count: usize,
    pub message: String,
}

/// POST /api/devices/{port}/preempt request.
#[derive(Debug, Deserialize)]
pub struct DevicePreemptRequest {
    /// Mandatory reason for preemption.
    pub reason: String,
    /// Client identifier (auto-generated if omitted)
    pub client_id: Option<String>,
}

/// POST /api/devices/{port}/preempt response.
#[derive(Debug, Serialize)]
pub struct DevicePreemptResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preempted_client_id: Option<String>,
    pub message: String,
}

/// GET /api/devices/{port}/status response.
#[derive(Debug, Serialize)]
pub struct DeviceStatusResponse {
    pub success: bool,
    pub port: String,
    pub device_id: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u16>,
    /// Human-readable USB vendor name (only present for USB ports).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_name: Option<String>,
    /// Human-readable USB product name (only present for USB ports).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_name: Option<String>,
    /// `true` for CDC-ACM, `false` for a USB-serial bridge, `null` if
    /// the host could not classify this port.
    pub is_cdc: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_port: Option<String>,
    pub is_connected: bool,
    pub available_for_exclusive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_holder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_lease: Option<DeviceLeaseInfo>,
    pub monitor_count: usize,
    pub monitor_leases: Vec<DeviceLeaseInfo>,
}

/// POST /api/test-emu — build firmware then run it in an emulator.
#[derive(Debug, Deserialize)]
pub struct TestEmuRequest {
    pub project_dir: String,
    pub environment: Option<String>,
    #[serde(default)]
    pub verbose: bool,
    pub timeout: Option<f64>,
    pub halt_on_error: Option<String>,
    pub halt_on_success: Option<String>,
    pub expect: Option<String>,
    /// Explicit emulator backend: "qemu" or "avr8js". Auto-detected if omitted.
    pub emulator: Option<String>,
    #[serde(default = "default_true")]
    pub show_timestamp: bool,
    pub request_id: Option<String>,
    pub caller_pid: Option<u32>,
    pub caller_cwd: Option<String>,
    /// Snapshot of `PLATFORMIO_*` env vars from the CLI caller's environment.
    #[serde(default)]
    pub pio_env: BTreeMap<String, String>,
    /// Optional explicit build-dir root override (see [`BuildRequest::build_dir_override`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_dir_override: Option<String>,
    /// Drop the `<env>` segment of the build-dir path (see [`BuildRequest::flatten_env`]).
    #[serde(default)]
    pub flatten_env: bool,
}

/// GET /api/cache/stats response.
#[derive(Debug, Default, Serialize)]
pub struct CacheStatsResponse {
    pub success: bool,
    pub archive_bytes: u64,
    pub installed_bytes: u64,
    pub total_bytes: u64,
    pub entry_count: i64,
    pub high_watermark: u64,
    pub low_watermark: u64,
    pub archive_budget: u64,
    pub installed_budget: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// POST /api/cache/gc response.
#[derive(Debug, Default, Serialize)]
pub struct GcResponse {
    pub success: bool,
    pub installed_evicted: u64,
    pub installed_bytes_freed: u64,
    pub archives_evicted: u64,
    pub archive_bytes_freed: u64,
    pub total_bytes_freed: u64,
    pub orphan_files_removed: usize,
    pub orphan_rows_cleaned: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// POST /api/reset request.
#[derive(Debug, Deserialize)]
pub struct ResetRequest {
    /// Serial port (e.g. "COM3", "/dev/ttyUSB0").
    pub port: String,
    /// Board identifier from platformio.ini (e.g. "esp32dev", "teensy40", "uno").
    /// Used to determine platform-specific reset sequence.
    /// If omitted, "generic" DTR toggle is used.
    pub board: Option<String>,
    #[serde(default)]
    pub verbose: bool,
    pub request_id: Option<String>,
    pub caller_pid: Option<u32>,
    pub caller_cwd: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- BuildRequest deserialization ---

    #[test]
    fn build_request_clean_build_field() {
        let json = r#"{"project_dir": "/tmp/p", "clean_build": true, "clean_all": true}"#;
        let req: BuildRequest = serde_json::from_str(json).unwrap();
        assert!(req.clean_build);
        assert!(req.clean_all);
        assert_eq!(req.project_dir, "/tmp/p");
    }

    #[test]
    fn build_request_clean_alias() {
        let json = r#"{"project_dir": "/tmp/p", "clean": true}"#;
        let req: BuildRequest = serde_json::from_str(json).unwrap();
        assert!(req.clean_build);
    }

    #[test]
    fn build_request_defaults() {
        let json = r#"{"project_dir": "/tmp/p"}"#;
        let req: BuildRequest = serde_json::from_str(json).unwrap();
        assert!(!req.clean_build);
        assert!(!req.clean_all);
        assert!(!req.clean_only);
        assert!(!req.verbose);
        assert!(req.environment.is_none());
        assert!(req.jobs.is_none());
        assert!(req.profile.is_none());
        assert!(req.request_id.is_none());
        assert!(req.src_dir.is_none());
    }

    #[test]
    fn build_request_src_dir_override() {
        let json = r#"{"project_dir": "/tmp/p", "src_dir": "examples/AutoResearch"}"#;
        let req: BuildRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.src_dir.unwrap(), "examples/AutoResearch");
    }

    #[test]
    fn build_request_with_request_id() {
        let json = r#"{"project_dir": "/tmp/p", "request_id": "abc-123"}"#;
        let req: BuildRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.request_id.unwrap(), "abc-123");
    }

    // --- DeployRequest deserialization ---

    #[test]
    fn deploy_request_all_fields() {
        let json = r#"{
            "project_dir": "/tmp/p",
            "port": "COM3",
            "monitor_after": true,
            "skip_build": true,
            "clean_build": true,
            "clean_all": true,
            "verbose": true,
            "monitor_timeout": 30.0,
            "monitor_halt_on_error": "FAIL",
            "monitor_halt_on_success": "PASS",
            "monitor_expect": "ready",
            "monitor_show_timestamp": false,
            "no_probe_rs": true,
            "request_id": "deploy-1"
        }"#;
        let req: DeployRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.project_dir, "/tmp/p");
        assert_eq!(req.port.unwrap(), "COM3");
        assert!(req.monitor_after);
        assert!(req.skip_build);
        assert!(req.clean_build);
        assert!(req.clean_all);
        assert!(req.verbose);
        assert_eq!(req.monitor_timeout.unwrap(), 30.0);
        assert_eq!(req.monitor_halt_on_error.unwrap(), "FAIL");
        assert_eq!(req.monitor_halt_on_success.unwrap(), "PASS");
        assert_eq!(req.monitor_expect.unwrap(), "ready");
        assert!(!req.monitor_show_timestamp);
        assert!(req.no_probe_rs);
        assert_eq!(req.request_id.unwrap(), "deploy-1");
    }

    #[test]
    fn deploy_request_clean_alias() {
        let json = r#"{"project_dir": "/tmp/p", "clean": true}"#;
        let req: DeployRequest = serde_json::from_str(json).unwrap();
        assert!(req.clean_build);
    }

    #[test]
    fn deploy_request_defaults() {
        let json = r#"{"project_dir": "/tmp/p"}"#;
        let req: DeployRequest = serde_json::from_str(json).unwrap();
        assert!(!req.monitor_after);
        assert!(!req.skip_build);
        assert!(!req.clean_build);
        assert!(!req.clean_all);
        assert!(!req.verbose);
        assert!(req.monitor_timeout.is_none());
        assert!(req.monitor_halt_on_error.is_none());
        assert!(req.monitor_show_timestamp);
        assert!(req.to.is_none());
        assert!(req.emulator.is_none());
        assert!(!req.no_probe_rs);
        assert!(!req.qemu);
        assert_eq!(req.qemu_timeout, 30);
        assert!(req.src_dir.is_none());
    }

    #[test]
    fn deploy_request_emulator_destination_fields() {
        let json = r#"{"project_dir": "/tmp/p", "to": "emu", "emulator": "avr8js"}"#;
        let req: DeployRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.to.as_deref(), Some("emu"));
        assert_eq!(req.emulator.as_deref(), Some("avr8js"));
    }

    #[test]
    fn deploy_request_src_dir_override() {
        let json = r#"{"project_dir": "/tmp/p", "src_dir": "examples/AutoResearch"}"#;
        let req: DeployRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.src_dir.unwrap(), "examples/AutoResearch");
    }

    // --- MonitorRequest deserialization ---

    #[test]
    fn monitor_request_all_fields() {
        let json = r#"{
            "project_dir": "/tmp/p",
            "port": "/dev/ttyUSB0",
            "baud_rate": 115200,
            "halt_on_error": "error",
            "halt_on_success": "ok",
            "expect": "boot",
            "timeout": 60.0,
            "request_id": "mon-1"
        }"#;
        let req: MonitorRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.port.unwrap(), "/dev/ttyUSB0");
        assert_eq!(req.baud_rate.unwrap(), 115200);
        assert_eq!(req.halt_on_error.unwrap(), "error");
        assert_eq!(req.halt_on_success.unwrap(), "ok");
        assert_eq!(req.expect.unwrap(), "boot");
        assert_eq!(req.timeout.unwrap(), 60.0);
        assert_eq!(req.request_id.unwrap(), "mon-1");
    }

    // --- OperationResponse serialization ---

    #[test]
    fn operation_response_ok_helper() {
        let resp = OperationResponse::ok("id-1".into(), "done".into());
        assert!(resp.success);
        assert_eq!(resp.exit_code, 0);
        assert!(resp.output_file.is_none());
    }

    #[test]
    fn operation_response_fail_helper() {
        let resp = OperationResponse::fail("id-2".into(), "broke".into());
        assert!(!resp.success);
        assert_eq!(resp.exit_code, 1);
    }

    #[test]
    fn operation_response_output_file_skipped_when_none() {
        let resp = OperationResponse::ok("id".into(), "ok".into());
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("output_file"));
    }

    #[test]
    fn operation_response_output_file_present_when_some() {
        let mut resp = OperationResponse::ok("id".into(), "ok".into());
        resp.output_file = Some("/tmp/fw.bin".into());
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("output_file"));
        assert!(json.contains("/tmp/fw.bin"));
    }

    /// ISSUES.md "Issue H": the success path of `/api/deploy` must
    /// forward esptool/avrdude `stdout` and `stderr` when present, so
    /// AI agents and CLI consumers can read flash progress, chip
    /// detection output, and similar diagnostics. The model layer must
    /// serialize them when set, and skip them when None (so quiet
    /// builds don't bloat the JSON).
    #[test]
    fn operation_response_stdout_stderr_round_trip_on_success() {
        let mut resp = OperationResponse::ok("id".into(), "deploy succeeded".into());
        resp.stdout = Some("Connecting....\nWriting at 0x10000... (12 %)".into());
        resp.stderr = Some("esptool.py v4.6.2".into());

        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"stdout\""));
        assert!(json.contains("Connecting"));
        assert!(json.contains("\"stderr\""));
        assert!(json.contains("esptool.py"));
        // Success path is still success.
        assert!(json.contains("\"success\":true"));
    }

    #[test]
    fn operation_response_stdout_stderr_skipped_when_none() {
        let resp = OperationResponse::ok("id".into(), "ok".into());
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("stdout"));
        assert!(!json.contains("stderr"));
    }

    // --- HealthResponse serialization ---

    #[test]
    fn health_response_uses_uptime_seconds() {
        let resp = HealthResponse {
            status: "healthy".into(),
            uptime_seconds: 42.5,
            version: "2.0.0".into(),
            pid: 1234,
            source_mtime: 1700000000.0,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("uptime_seconds"));
        assert!(!json.contains("uptime_secs"));
        assert!(json.contains("\"healthy\""));
    }

    // --- DaemonInfoResponse serialization ---

    #[test]
    fn daemon_info_response_has_new_fields() {
        let resp = DaemonInfoResponse {
            status: "running".into(),
            uptime_seconds: 10.0,
            version: "2.0.0".into(),
            pid: 5678,
            port: 8765,
            started_at: 1700000000.0,
            dev_mode: false,
            host: "127.0.0.1".into(),
            operation_in_progress: false,
            daemon_state: fbuild_core::DaemonState::Idle,
            current_operation: None,
            dependency_install: None,
            client_count: 3,
            cache_dir: "/home/user/.fbuild/prod/cache".into(),
            cache_identity:
                "mode=prod;trust=local-shared;schema=1;cache=/home/user/.fbuild/prod/cache".into(),
            cache_schema_version: 1,
            daemon_dir: "/home/user/.fbuild/prod/daemon".into(),
            source_mtime: 1700000000.0,
            spawner_cwd: "/home/user/project".into(),
            mcp_url: "http://127.0.0.1:8765/mcp".into(),
            watch_set_cache: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"started_at\""));
        assert!(json.contains("\"dev_mode\""));
        assert!(json.contains("\"host\""));
        assert!(json.contains("\"127.0.0.1\""));
        assert!(json.contains("uptime_seconds"));
        assert!(json.contains("\"operation_in_progress\""));
        assert!(json.contains("\"daemon_state\""));
        assert!(json.contains("\"idle\""));
        assert!(json.contains("\"client_count\":3"));
        assert!(json.contains("\"cache_dir\""));
        assert!(json.contains("\"cache_identity\""));
        assert!(json.contains("\"cache_schema_version\":1"));
        assert!(json.contains("\"daemon_dir\""));
        assert!(json.contains("\"source_mtime\""));
        assert!(json.contains("\"spawner_cwd\""));
        assert!(json.contains("\"mcp_url\""));
    }

    #[test]
    fn daemon_info_response_current_operation_skipped_when_none() {
        let resp = DaemonInfoResponse {
            status: "running".into(),
            uptime_seconds: 10.0,
            version: "2.0.0".into(),
            pid: 5678,
            port: 8765,
            started_at: 1700000000.0,
            dev_mode: false,
            host: "127.0.0.1".into(),
            operation_in_progress: false,
            daemon_state: fbuild_core::DaemonState::Idle,
            current_operation: None,
            dependency_install: None,
            client_count: 0,
            cache_dir: "/tmp/cache".into(),
            cache_identity: "mode=prod;trust=local-shared;schema=1;cache=/tmp/cache".into(),
            cache_schema_version: 1,
            daemon_dir: "/tmp/daemon".into(),
            source_mtime: 0.0,
            spawner_cwd: "unknown".into(),
            mcp_url: "http://127.0.0.1:8765/mcp".into(),
            watch_set_cache: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("current_operation"));
    }

    #[test]
    fn daemon_info_response_current_operation_present_when_some() {
        let resp = DaemonInfoResponse {
            status: "running".into(),
            uptime_seconds: 10.0,
            version: "2.0.0".into(),
            pid: 5678,
            port: 8765,
            started_at: 1700000000.0,
            dev_mode: false,
            host: "127.0.0.1".into(),
            operation_in_progress: true,
            daemon_state: fbuild_core::DaemonState::Building,
            current_operation: Some("Building /tmp/myproject".into()),
            dependency_install: Some(fbuild_core::install_status::status(
                "zccache",
                Some("1.12.9"),
                fbuild_core::install_status::InstallPhase::WaitingForLock,
                fbuild_core::install_status::InstallRole::Waiter,
                "waiting for managed zccache",
                Some(".zccache-1.12.9.install.lock"),
            )),
            client_count: 1,
            cache_dir: "/tmp/cache".into(),
            cache_identity: "mode=prod;trust=local-shared;schema=1;cache=/tmp/cache".into(),
            cache_schema_version: 1,
            daemon_dir: "/tmp/daemon".into(),
            source_mtime: 0.0,
            spawner_cwd: "unknown".into(),
            mcp_url: "http://127.0.0.1:8765/mcp".into(),
            watch_set_cache: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"current_operation\""));
        assert!(json.contains("Building /tmp/myproject"));
        assert!(json.contains("\"building\""));
        assert!(json.contains("\"dependency_install\""));
        assert!(json.contains("\"waiting_for_lock\""));
    }

    // --- DeviceListResponse ---

    #[test]
    fn device_list_response_has_success() {
        let resp = DeviceListResponse {
            success: true,
            devices: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
    }

    // --- ShutdownParams ---

    #[test]
    fn shutdown_params_force_true() {
        let json = r#"{"force": true}"#;
        let params: ShutdownParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.force, Some(true));
    }

    #[test]
    fn shutdown_params_empty() {
        let json = "{}";
        let params: ShutdownParams = serde_json::from_str(json).unwrap();
        assert!(params.force.is_none());
    }

    // --- ResetRequest deserialization ---

    #[test]
    fn reset_request_minimal() {
        let json = r#"{"port": "COM3"}"#;
        let req: ResetRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.port, "COM3");
        assert!(req.board.is_none());
        assert!(!req.verbose);
        assert!(req.request_id.is_none());
    }

    #[test]
    fn reset_request_all_fields() {
        let json = r#"{"port": "/dev/ttyUSB0", "board": "esp32dev", "verbose": true, "request_id": "rst-1"}"#;
        let req: ResetRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.port, "/dev/ttyUSB0");
        assert_eq!(req.board.unwrap(), "esp32dev");
        assert!(req.verbose);
        assert_eq!(req.request_id.unwrap(), "rst-1");
    }

    // --- TestEmuRequest deserialization ---

    #[test]
    fn test_emu_request_minimal() {
        let json = r#"{"project_dir": "/tmp/p"}"#;
        let req: TestEmuRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.project_dir, "/tmp/p");
        assert!(req.environment.is_none());
        assert!(!req.verbose);
        assert!(req.timeout.is_none());
        assert!(req.halt_on_error.is_none());
        assert!(req.halt_on_success.is_none());
        assert!(req.expect.is_none());
        assert!(req.emulator.is_none());
        assert!(req.show_timestamp); // default true
        assert!(req.request_id.is_none());
    }

    #[test]
    fn test_emu_request_all_fields() {
        let json = r#"{
            "project_dir": "/tmp/p",
            "environment": "uno",
            "verbose": true,
            "timeout": 10.0,
            "halt_on_error": "FAIL",
            "halt_on_success": "PASS",
            "expect": "ready",
            "emulator": "avr8js",
            "show_timestamp": false,
            "request_id": "test-1"
        }"#;
        let req: TestEmuRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.project_dir, "/tmp/p");
        assert_eq!(req.environment.unwrap(), "uno");
        assert!(req.verbose);
        assert_eq!(req.timeout.unwrap(), 10.0);
        assert_eq!(req.halt_on_error.unwrap(), "FAIL");
        assert_eq!(req.halt_on_success.unwrap(), "PASS");
        assert_eq!(req.expect.unwrap(), "ready");
        assert_eq!(req.emulator.unwrap(), "avr8js");
        assert!(!req.show_timestamp);
        assert_eq!(req.request_id.unwrap(), "test-1");
    }
}
