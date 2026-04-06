//! Request/response JSON types matching the Python daemon's API contract.

use serde::{Deserialize, Serialize};

/// POST /api/build
#[derive(Debug, Deserialize)]
pub struct BuildRequest {
    pub project_dir: String,
    pub environment: Option<String>,
    #[serde(default, alias = "clean")]
    pub clean_build: bool,
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
}

/// POST /api/deploy
#[derive(Debug, Deserialize)]
pub struct DeployRequest {
    pub project_dir: String,
    pub environment: Option<String>,
    pub port: Option<String>,
    #[serde(default)]
    pub monitor_after: bool,
    #[serde(default)]
    pub skip_build: bool,
    #[serde(default, alias = "clean")]
    pub clean_build: bool,
    #[serde(default)]
    pub verbose: bool,
    pub monitor_timeout: Option<f64>,
    pub monitor_halt_on_error: Option<String>,
    pub monitor_halt_on_success: Option<String>,
    pub monitor_expect: Option<String>,
    #[serde(default = "default_true")]
    pub monitor_show_timestamp: bool,
    #[serde(default)]
    pub qemu: bool,
    #[serde(default = "default_qemu_timeout")]
    pub qemu_timeout: u32,
    pub request_id: Option<String>,
    pub caller_pid: Option<u32>,
    pub caller_cwd: Option<String>,
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
}

impl OperationResponse {
    pub fn ok(request_id: String, message: String) -> Self {
        Self {
            success: true,
            request_id,
            message,
            exit_code: 0,
            output_file: None,
        }
    }

    pub fn fail(request_id: String, message: String) -> Self {
        Self {
            success: false,
            request_id,
            message,
            exit_code: 1,
            output_file: None,
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
    pub client_count: usize,
    pub cache_dir: String,
    pub daemon_dir: String,
    pub source_mtime: f64,
    pub spawner_cwd: String,
    /// MCP (Model Context Protocol) server URL.
    pub mcp_url: String,
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
    pub description: String,
}

/// POST /api/devices/list response.
#[derive(Debug, Serialize)]
pub struct DeviceListResponse {
    pub success: bool,
    pub devices: Vec<DeviceInfo>,
}

/// GET /api/locks/status response.
#[derive(Debug, Serialize)]
pub struct LockStatusResponse {
    pub success: bool,
    pub port_locks: Vec<PortLockInfo>,
    pub project_locks: Vec<ProjectLockInfo>,
    pub stale_locks: Vec<String>,
}

/// Lock information for a serial port.
#[derive(Debug, Serialize)]
pub struct PortLockInfo {
    pub port: String,
    pub is_held: bool,
    pub holder_description: Option<String>,
    pub is_open: bool,
    pub writer_client_id: Option<String>,
    pub reader_count: usize,
}

/// Lock information for a project directory.
#[derive(Debug, Serialize)]
pub struct ProjectLockInfo {
    pub project_dir: String,
    pub is_held: bool,
}

/// POST /api/locks/clear response.
#[derive(Debug, Serialize)]
pub struct ClearLocksResponse {
    pub success: bool,
    pub cleared_count: usize,
    pub message: String,
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
    pub is_connected: bool,
    pub available_for_exclusive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_holder: Option<String>,
    pub monitor_count: usize,
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
        let json = r#"{"project_dir": "/tmp/p", "clean_build": true}"#;
        let req: BuildRequest = serde_json::from_str(json).unwrap();
        assert!(req.clean_build);
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
        assert!(!req.verbose);
        assert!(req.environment.is_none());
        assert!(req.jobs.is_none());
        assert!(req.profile.is_none());
        assert!(req.request_id.is_none());
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
            "verbose": true,
            "monitor_timeout": 30.0,
            "monitor_halt_on_error": "FAIL",
            "monitor_halt_on_success": "PASS",
            "monitor_expect": "ready",
            "monitor_show_timestamp": false,
            "request_id": "deploy-1"
        }"#;
        let req: DeployRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.project_dir, "/tmp/p");
        assert_eq!(req.port.unwrap(), "COM3");
        assert!(req.monitor_after);
        assert!(req.skip_build);
        assert!(req.clean_build);
        assert!(req.verbose);
        assert_eq!(req.monitor_timeout.unwrap(), 30.0);
        assert_eq!(req.monitor_halt_on_error.unwrap(), "FAIL");
        assert_eq!(req.monitor_halt_on_success.unwrap(), "PASS");
        assert_eq!(req.monitor_expect.unwrap(), "ready");
        assert!(!req.monitor_show_timestamp);
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
        assert!(!req.verbose);
        assert!(req.monitor_timeout.is_none());
        assert!(req.monitor_halt_on_error.is_none());
        assert!(req.monitor_show_timestamp);
        assert!(!req.qemu);
        assert_eq!(req.qemu_timeout, 30);
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
            client_count: 3,
            cache_dir: "/home/user/.fbuild/prod/cache".into(),
            daemon_dir: "/home/user/.fbuild/prod/daemon".into(),
            source_mtime: 1700000000.0,
            spawner_cwd: "/home/user/project".into(),
            mcp_url: "http://127.0.0.1:8765/mcp".into(),
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
            client_count: 0,
            cache_dir: "/tmp/cache".into(),
            daemon_dir: "/tmp/daemon".into(),
            source_mtime: 0.0,
            spawner_cwd: "unknown".into(),
            mcp_url: "http://127.0.0.1:8765/mcp".into(),
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
            client_count: 1,
            cache_dir: "/tmp/cache".into(),
            daemon_dir: "/tmp/daemon".into(),
            source_mtime: 0.0,
            spawner_cwd: "unknown".into(),
            mcp_url: "http://127.0.0.1:8765/mcp".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"current_operation\""));
        assert!(json.contains("Building /tmp/myproject"));
        assert!(json.contains("\"building\""));
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
}
