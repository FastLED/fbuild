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
    pub request_id: Option<String>,
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
    pub request_id: Option<String>,
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
    pub request_id: Option<String>,
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
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"started_at\""));
        assert!(json.contains("\"dev_mode\""));
        assert!(json.contains("\"host\""));
        assert!(json.contains("\"127.0.0.1\""));
        assert!(json.contains("uptime_seconds"));
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
}
