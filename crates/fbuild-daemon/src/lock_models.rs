//! Lock status and cleanup request/response models.

use serde::{Deserialize, Serialize};

/// GET /api/locks/status response.
#[derive(Debug, Serialize)]
pub struct LockStatusResponse {
    pub success: bool,
    pub port_locks: Vec<PortLockInfo>,
    pub project_locks: Vec<ProjectLockInfo>,
    pub pending_serial_attaches: Vec<PendingSerialAttachLockInfo>,
    pub stale_locks: Vec<String>,
}

/// Lock information for a serial port.
#[derive(Debug, Serialize)]
pub struct PortLockInfo {
    pub port: String,
    pub is_held: bool,
    pub holder_description: Option<String>,
    pub is_open: bool,
    pub owner_client_id: Option<String>,
    pub writer_client_id: Option<String>,
    pub reader_count: usize,
    pub reader_client_ids: Vec<String>,
    pub baud_rate: u32,
    pub started_at: f64,
    pub session_age_seconds: f64,
    pub last_activity_at: f64,
    pub last_activity_age_seconds: f64,
    pub last_read_at: Option<f64>,
    pub last_read_age_seconds: Option<f64>,
    pub last_write_at: Option<f64>,
    pub last_write_age_seconds: Option<f64>,
    pub total_bytes_read: u64,
    pub total_bytes_written: u64,
    pub clients: Vec<SerialClientLockInfo>,
}

/// Best-effort owner metadata for a serial session client.
#[derive(Debug, Serialize)]
pub struct SerialClientLockInfo {
    pub client_id: String,
    pub pid: Option<u32>,
    pub process_alive: Option<bool>,
    pub exe: Option<String>,
    pub cwd: Option<String>,
    pub argv: Option<Vec<String>>,
}

/// WebSocket attach currently in progress.
#[derive(Debug, Serialize)]
pub struct PendingSerialAttachLockInfo {
    pub id: u64,
    pub client_id: Option<String>,
    pub port: Option<String>,
    pub started_at: f64,
    pub age_seconds: f64,
}

/// Lock information for a project directory.
#[derive(Debug, Serialize)]
pub struct ProjectLockInfo {
    pub project_dir: String,
    pub is_held: bool,
}

/// POST /api/locks/clear request.
#[derive(Debug, Default, Deserialize)]
pub struct ClearLocksRequest {
    #[serde(default)]
    pub serial: bool,
    #[serde(default)]
    pub stale: bool,
    pub port: Option<String>,
    pub client_id: Option<String>,
    #[serde(default)]
    pub force: bool,
}

/// POST /api/locks/clear response.
#[derive(Debug, Serialize)]
pub struct ClearLocksResponse {
    pub success: bool,
    pub cleared_count: usize,
    pub cleared_project_count: usize,
    pub cleared_serial_count: usize,
    pub cleared_serial_sessions: Vec<String>,
    pub refused: Vec<String>,
    pub message: String,
}
