//! Persistent daemon status file (`daemon_status.json`).
//!
//! Mirrors the Python `StatusManager` — writes a JSON file that CLI processes
//! can read without an HTTP roundtrip to discover the daemon's current state.

use fbuild_core::DaemonState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

/// Serializable snapshot of daemon status, written to `daemon_status.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub state: DaemonState,
    pub message: String,
    pub updated_at: f64,

    #[serde(default)]
    pub operation_in_progress: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_started_at: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_started_at: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_operation: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
}

impl DaemonStatus {
    /// Create a default IDLE status.
    pub fn idle(pid: u32, started_at: f64) -> Self {
        Self {
            state: DaemonState::Idle,
            message: "Daemon idle".to_string(),
            updated_at: now_unix(),
            operation_in_progress: false,
            daemon_pid: Some(pid),
            daemon_started_at: Some(started_at),
            caller_pid: None,
            caller_cwd: None,
            request_id: None,
            request_started_at: None,
            environment: None,
            project_dir: None,
            current_operation: None,
            exit_code: None,
            port: None,
        }
    }
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Context for an in-progress operation (avoids too-many-arguments).
#[derive(Default)]
pub struct OperationInfo<'a> {
    pub state: DaemonState,
    pub message: &'a str,
    pub project_dir: Option<&'a str>,
    pub environment: Option<&'a str>,
    pub current_operation: Option<&'a str>,
    pub caller_pid: Option<u32>,
    pub caller_cwd: Option<&'a str>,
    pub request_id: Option<&'a str>,
    pub serial_port: Option<&'a str>,
}

/// Thread-safe manager for reading/writing `daemon_status.json`.
pub struct StatusManager {
    path: PathBuf,
    /// Mutex-protected current status for atomic read-modify-write.
    current: Mutex<DaemonStatus>,
}

impl StatusManager {
    /// Create a new `StatusManager` that writes to the given path.
    pub fn new(path: PathBuf, pid: u32, started_at: f64) -> Self {
        let status = DaemonStatus::idle(pid, started_at);
        let mgr = Self {
            path,
            current: Mutex::new(status),
        };
        mgr.flush();
        mgr
    }

    /// Update status fields and write to disk.
    pub fn update(&self, state: DaemonState, message: &str, operation_in_progress: bool) {
        if let Ok(mut s) = self.current.lock() {
            s.state = state;
            s.message = message.to_string();
            s.updated_at = now_unix();
            s.operation_in_progress = operation_in_progress;
            self.write_atomic(&s);
        }
    }

    /// Update with full operation context.
    pub fn update_operation(&self, info: &OperationInfo<'_>) {
        if let Ok(mut s) = self.current.lock() {
            s.state = info.state;
            s.message = info.message.to_string();
            s.updated_at = now_unix();
            s.operation_in_progress = true;
            s.project_dir = info.project_dir.map(|v| v.to_string());
            s.environment = info.environment.map(|v| v.to_string());
            s.current_operation = info.current_operation.map(|v| v.to_string());
            s.caller_pid = info.caller_pid;
            s.caller_cwd = info.caller_cwd.map(|v| v.to_string());
            s.request_id = info.request_id.map(|v| v.to_string());
            s.request_started_at = Some(now_unix());
            s.port = info.serial_port.map(|v| v.to_string());
            s.exit_code = None;
            self.write_atomic(&s);
        }
    }

    /// Record operation completion.
    pub fn complete(&self, state: DaemonState, message: &str, exit_code: i32) {
        if let Ok(mut s) = self.current.lock() {
            s.state = state;
            s.message = message.to_string();
            s.updated_at = now_unix();
            s.operation_in_progress = false;
            s.exit_code = Some(exit_code);
            s.current_operation = None;
            self.write_atomic(&s);
        }
    }

    /// Reset to idle.
    pub fn set_idle(&self) {
        if let Ok(mut s) = self.current.lock() {
            s.state = DaemonState::Idle;
            s.message = "Daemon idle".to_string();
            s.updated_at = now_unix();
            s.operation_in_progress = false;
            s.exit_code = None;
            s.current_operation = None;
            s.project_dir = None;
            s.environment = None;
            s.caller_pid = None;
            s.caller_cwd = None;
            s.request_id = None;
            s.request_started_at = None;
            s.port = None;
            self.write_atomic(&s);
        }
    }

    /// Read status from disk (for testing or external consumers).
    pub fn read_from_disk(&self) -> Option<DaemonStatus> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    /// Remove the status file (cleanup on daemon exit).
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    /// Flush current in-memory status to disk.
    fn flush(&self) {
        if let Ok(s) = self.current.lock() {
            self.write_atomic(&s);
        }
    }

    /// Atomic write: route through the canonical async helper
    /// `fbuild_core::fs::write_atomic` so the daemon-side state file
    /// uses the same write-tempfile / fsync / rename path as every
    /// other state-file writer (FastLED/fbuild#844 bridge pair 6).
    ///
    /// Called from synchronous code paths inside the tokio runtime, so
    /// we bridge to the async helper via `block_in_place` +
    /// `Handle::block_on`. Matches the pattern in
    /// `fbuild_packages::toolchain::esp32_metadata::resolve_toolchain_url_sync`.
    fn write_atomic(&self, status: &DaemonStatus) {
        let json = match serde_json::to_string_pretty(status) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("failed to serialize daemon status: {}", e);
                return;
            }
        };

        // Ensure parent directory exists. `fbuild_core::fs::write_atomic`
        // probes for it and errors if missing — surfacing that as a
        // simple `create_dir_all` keeps the daemon's bootstrap behaviour.
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let path = self.path.clone();
        let result = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            tokio::task::block_in_place(|| {
                handle.block_on(fbuild_core::fs::write_atomic(&path, json.as_bytes()))
            })
        } else {
            // No tokio runtime — spin up a one-shot. Path hit only by
            // unit tests and the (rare) standalone bootstrap before the
            // daemon's runtime exists.
            match tokio::runtime::Runtime::new() {
                Ok(rt) => rt.block_on(fbuild_core::fs::write_atomic(&path, json.as_bytes())),
                Err(e) => {
                    tracing::warn!("failed to create tokio runtime for status write: {}", e);
                    return;
                }
            }
        };
        if let Err(e) = result {
            tracing::warn!(
                "failed to atomically write daemon status to {}: {}",
                self.path.display(),
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_mgr() -> (StatusManager, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("daemon_status.json");
        let mgr = StatusManager::new(path, 1234, 1700000000.0);
        (mgr, dir)
    }

    #[test]
    fn new_writes_idle_status() {
        let (mgr, _dir) = make_mgr();
        let status = mgr.read_from_disk().unwrap();
        assert_eq!(status.state, DaemonState::Idle);
        assert_eq!(status.daemon_pid, Some(1234));
        assert!(!status.operation_in_progress);
    }

    #[test]
    fn update_changes_state() {
        let (mgr, _dir) = make_mgr();
        mgr.update(DaemonState::Building, "Building project", true);
        let status = mgr.read_from_disk().unwrap();
        assert_eq!(status.state, DaemonState::Building);
        assert_eq!(status.message, "Building project");
        assert!(status.operation_in_progress);
    }

    #[test]
    fn update_operation_sets_all_fields() {
        let (mgr, _dir) = make_mgr();
        mgr.update_operation(&OperationInfo {
            state: DaemonState::Deploying,
            message: "Deploying esp32",
            project_dir: Some("/tmp/project"),
            environment: Some("esp32c6"),
            current_operation: Some("Uploading firmware"),
            caller_pid: Some(9999),
            caller_cwd: Some("/home/user"),
            request_id: Some("deploy-123"),
            serial_port: Some("COM3"),
        });
        let status = mgr.read_from_disk().unwrap();
        assert_eq!(status.state, DaemonState::Deploying);
        assert!(status.operation_in_progress);
        assert_eq!(status.project_dir.as_deref(), Some("/tmp/project"));
        assert_eq!(status.environment.as_deref(), Some("esp32c6"));
        assert_eq!(
            status.current_operation.as_deref(),
            Some("Uploading firmware")
        );
        assert_eq!(status.caller_pid, Some(9999));
        assert_eq!(status.port.as_deref(), Some("COM3"));
    }

    #[test]
    fn complete_clears_operation() {
        let (mgr, _dir) = make_mgr();
        mgr.update(DaemonState::Building, "Building", true);
        mgr.complete(DaemonState::Completed, "Build succeeded", 0);
        let status = mgr.read_from_disk().unwrap();
        assert_eq!(status.state, DaemonState::Completed);
        assert!(!status.operation_in_progress);
        assert_eq!(status.exit_code, Some(0));
        assert!(status.current_operation.is_none());
    }

    #[test]
    fn set_idle_resets_everything() {
        let (mgr, _dir) = make_mgr();
        mgr.update_operation(&OperationInfo {
            state: DaemonState::Building,
            message: "Building",
            project_dir: Some("/tmp/p"),
            environment: Some("avr"),
            current_operation: Some("Compiling"),
            caller_pid: Some(42),
            caller_cwd: Some("/home"),
            request_id: Some("req-1"),
            serial_port: Some("COM1"),
        });
        mgr.set_idle();
        let status = mgr.read_from_disk().unwrap();
        assert_eq!(status.state, DaemonState::Idle);
        assert!(!status.operation_in_progress);
        assert!(status.project_dir.is_none());
        assert!(status.environment.is_none());
        assert!(status.caller_pid.is_none());
        assert!(status.port.is_none());
    }

    #[test]
    fn remove_deletes_file() {
        let (mgr, dir) = make_mgr();
        let path = dir.path().join("daemon_status.json");
        assert!(path.exists());
        mgr.remove();
        assert!(!path.exists());
    }

    #[test]
    fn read_from_disk_returns_none_on_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.json");
        let mgr = StatusManager {
            path,
            current: Mutex::new(DaemonStatus::idle(1, 0.0)),
        };
        assert!(mgr.read_from_disk().is_none());
    }
}
