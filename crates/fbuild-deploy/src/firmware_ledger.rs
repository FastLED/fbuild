//! Firmware deployment ledger — tracks what firmware is deployed on each port
//! to skip unnecessary re-uploads when source hasn't changed.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing;

/// How long before a ledger entry is considered stale (24 hours).
const STALE_THRESHOLD_SECS: f64 = 24.0 * 3600.0;

/// Source file extensions to include in hash computation.
const SOURCE_EXTENSIONS: &[&str] = &["c", "cpp", "h", "hpp", "ino", "S"];

/// A single firmware deployment record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirmwareEntry {
    pub port: String,
    pub firmware_hash: String,
    pub source_hash: String,
    pub project_dir: String,
    pub environment: String,
    pub upload_timestamp: f64,
    #[serde(default)]
    pub build_flags_hash: Option<String>,
}

impl FirmwareEntry {
    /// Check if this entry is older than the stale threshold.
    pub fn is_stale(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        (now - self.upload_timestamp) > STALE_THRESHOLD_SECS
    }
}

/// Thread-safe firmware deployment ledger backed by a JSON file.
pub struct FirmwareLedger {
    path: PathBuf,
    entries: Mutex<HashMap<String, FirmwareEntry>>,
}

impl Default for FirmwareLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl FirmwareLedger {
    /// Create a new ledger stored at `~/.fbuild/{dev|prod}/firmware_ledger.json`.
    pub fn new() -> Self {
        let path = fbuild_paths::get_fbuild_root().join("firmware_ledger.json");
        let entries = Self::load_from_disk(&path);
        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    fn load_from_disk(path: &Path) -> HashMap<String, FirmwareEntry> {
        match std::fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => HashMap::new(),
        }
    }

    fn save_to_disk(&self, entries: &HashMap<String, FirmwareEntry>) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(entries) {
            Ok(data) => {
                if let Err(e) = std::fs::write(&self.path, data) {
                    tracing::warn!("failed to save firmware ledger: {}", e);
                }
            }
            Err(e) => tracing::warn!("failed to serialize firmware ledger: {}", e),
        }
    }

    /// Record a successful firmware deployment.
    pub fn record_deployment(
        &self,
        port: &str,
        firmware_hash: &str,
        source_hash: &str,
        project_dir: &str,
        environment: &str,
        build_flags_hash: Option<&str>,
    ) {
        let entry = FirmwareEntry {
            port: port.to_string(),
            firmware_hash: firmware_hash.to_string(),
            source_hash: source_hash.to_string(),
            project_dir: project_dir.to_string(),
            environment: environment.to_string(),
            upload_timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
            build_flags_hash: build_flags_hash.map(|s| s.to_string()),
        };
        let mut entries = self.entries.lock().unwrap();
        entries.insert(port.to_string(), entry);
        self.save_to_disk(&entries);
    }

    /// Get the deployment record for a port, or None if stale/missing.
    pub fn get_deployment(&self, port: &str) -> Option<FirmwareEntry> {
        let entries = self.entries.lock().unwrap();
        entries
            .get(port)
            .and_then(|e| if e.is_stale() { None } else { Some(e.clone()) })
    }

    /// Check whether the firmware on `port` needs to be redeployed.
    /// Returns `true` if a redeploy is required.
    pub fn needs_redeploy(
        &self,
        port: &str,
        source_hash: &str,
        build_flags_hash: Option<&str>,
    ) -> bool {
        match self.get_deployment(port) {
            None => true,
            Some(entry) => {
                if entry.source_hash != source_hash {
                    tracing::info!("firmware ledger: source hash changed for port {}", port);
                    return true;
                }
                if entry.build_flags_hash.as_deref() != build_flags_hash {
                    tracing::info!("firmware ledger: build flags changed for port {}", port);
                    return true;
                }
                tracing::info!(
                    "firmware ledger: firmware on {} is current, skipping redeploy",
                    port
                );
                false
            }
        }
    }

    /// Clear the entry for a specific port.
    pub fn clear(&self, port: &str) {
        let mut entries = self.entries.lock().unwrap();
        entries.remove(port);
        self.save_to_disk(&entries);
    }

    /// Clear all entries.
    pub fn clear_all(&self) {
        let mut entries = self.entries.lock().unwrap();
        entries.clear();
        self.save_to_disk(&entries);
    }

    /// Remove stale entries.
    pub fn clear_stale(&self) {
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, e| !e.is_stale());
        self.save_to_disk(&entries);
    }

    /// Get all non-stale entries.
    pub fn get_all(&self) -> Vec<FirmwareEntry> {
        let entries = self.entries.lock().unwrap();
        entries
            .values()
            .filter(|e| !e.is_stale())
            .cloned()
            .collect()
    }
}

/// Compute SHA256 hash of a firmware binary file.
pub fn compute_firmware_hash(path: &Path) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let data = std::fs::read(path)?;
    hasher.update(&data);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Compute a combined SHA256 hash of all source files in a project.
///
/// Includes files from `src/`, `include/`, `lib/`, and `*.ino` in root.
/// Matches the Python implementation's behavior of hashing sorted paths + contents.
pub fn compute_source_hash(project_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    let mut files = collect_source_files(project_dir);
    files.sort();

    for file in &files {
        // Include the relative path as a delimiter (matching Python)
        if let Ok(rel) = file.strip_prefix(project_dir) {
            hasher.update(rel.to_string_lossy().as_bytes());
        }
        if let Ok(data) = std::fs::read(file) {
            hasher.update(&data);
        }
    }

    format!("{:x}", hasher.finalize())
}

/// Compute SHA256 hash of sorted build flags.
pub fn compute_build_flags_hash(flags: &[String]) -> String {
    let mut hasher = Sha256::new();
    let mut sorted = flags.to_vec();
    sorted.sort();
    for flag in &sorted {
        hasher.update(flag.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Collect source files from standard PlatformIO directories.
fn collect_source_files(project_dir: &Path) -> Vec<PathBuf> {
    let dirs = ["src", "include", "lib"];
    let mut files = Vec::new();

    for dir_name in &dirs {
        let dir = project_dir.join(dir_name);
        if dir.is_dir() {
            collect_files_recursive(&dir, &mut files);
        }
    }

    // Also include *.ino files in root
    if let Ok(entries) = std::fs::read_dir(project_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if ext == "ino" {
                        files.push(path);
                    }
                }
            }
        }
    }

    files
}

fn collect_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(&path, files);
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if SOURCE_EXTENSIONS.contains(&ext) {
                    files.push(path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn firmware_entry_not_stale_when_fresh() {
        let entry = FirmwareEntry {
            port: "COM3".to_string(),
            firmware_hash: "abc123".to_string(),
            source_hash: "def456".to_string(),
            project_dir: "/tmp/test".to_string(),
            environment: "esp32dev".to_string(),
            upload_timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
            build_flags_hash: None,
        };
        assert!(!entry.is_stale());
    }

    #[test]
    fn firmware_entry_stale_after_threshold() {
        let entry = FirmwareEntry {
            port: "COM3".to_string(),
            firmware_hash: "abc123".to_string(),
            source_hash: "def456".to_string(),
            project_dir: "/tmp/test".to_string(),
            environment: "esp32dev".to_string(),
            upload_timestamp: 1000.0, // Very old timestamp
            build_flags_hash: None,
        };
        assert!(entry.is_stale());
    }

    #[test]
    fn ledger_record_and_retrieve() {
        let tmp = TempDir::new().unwrap();
        let ledger_path = tmp.path().join("firmware_ledger.json");
        let ledger = FirmwareLedger {
            path: ledger_path,
            entries: Mutex::new(HashMap::new()),
        };

        ledger.record_deployment("COM3", "fw_hash", "src_hash", "/project", "esp32dev", None);

        let entry = ledger.get_deployment("COM3").unwrap();
        assert_eq!(entry.firmware_hash, "fw_hash");
        assert_eq!(entry.source_hash, "src_hash");
        assert_eq!(entry.project_dir, "/project");
        assert_eq!(entry.environment, "esp32dev");
    }

    #[test]
    fn ledger_needs_redeploy_on_source_change() {
        let tmp = TempDir::new().unwrap();
        let ledger_path = tmp.path().join("firmware_ledger.json");
        let ledger = FirmwareLedger {
            path: ledger_path,
            entries: Mutex::new(HashMap::new()),
        };

        ledger.record_deployment(
            "COM3",
            "fw_hash",
            "src_hash_v1",
            "/project",
            "esp32dev",
            Some("flags_hash"),
        );

        // Same source + flags → no redeploy
        assert!(!ledger.needs_redeploy("COM3", "src_hash_v1", Some("flags_hash")));

        // Changed source → needs redeploy
        assert!(ledger.needs_redeploy("COM3", "src_hash_v2", Some("flags_hash")));

        // Changed flags → needs redeploy
        assert!(ledger.needs_redeploy("COM3", "src_hash_v1", Some("flags_hash_v2")));

        // Unknown port → needs redeploy
        assert!(ledger.needs_redeploy("COM4", "src_hash_v1", Some("flags_hash")));
    }

    #[test]
    fn ledger_clear_and_clear_all() {
        let tmp = TempDir::new().unwrap();
        let ledger_path = tmp.path().join("firmware_ledger.json");
        let ledger = FirmwareLedger {
            path: ledger_path,
            entries: Mutex::new(HashMap::new()),
        };

        ledger.record_deployment("COM3", "h1", "s1", "/p1", "e1", None);
        ledger.record_deployment("COM4", "h2", "s2", "/p2", "e2", None);

        ledger.clear("COM3");
        assert!(ledger.get_deployment("COM3").is_none());
        assert!(ledger.get_deployment("COM4").is_some());

        ledger.clear_all();
        assert!(ledger.get_deployment("COM4").is_none());
    }

    #[test]
    fn compute_source_hash_empty_project() {
        let tmp = TempDir::new().unwrap();
        let hash = compute_source_hash(tmp.path());
        // Empty project should produce a consistent hash
        assert!(!hash.is_empty());
    }

    #[test]
    fn compute_source_hash_changes_with_content() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir(&src_dir).unwrap();

        std::fs::write(src_dir.join("main.cpp"), "void setup() {}").unwrap();
        let hash1 = compute_source_hash(tmp.path());

        std::fs::write(src_dir.join("main.cpp"), "void setup() { changed(); }").unwrap();
        let hash2 = compute_source_hash(tmp.path());

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn compute_firmware_hash_works() {
        let tmp = TempDir::new().unwrap();
        let fw_path = tmp.path().join("firmware.bin");
        std::fs::write(&fw_path, b"fake firmware data").unwrap();

        let hash = compute_firmware_hash(&fw_path).unwrap();
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 hex
    }

    #[test]
    fn compute_build_flags_hash_order_independent() {
        let hash1 = compute_build_flags_hash(&["-O2".to_string(), "-DFOO".to_string()]);
        let hash2 = compute_build_flags_hash(&["-DFOO".to_string(), "-O2".to_string()]);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn ledger_persists_to_disk() {
        let tmp = TempDir::new().unwrap();
        let ledger_path = tmp.path().join("firmware_ledger.json");

        // Create and record
        {
            let ledger = FirmwareLedger {
                path: ledger_path.clone(),
                entries: Mutex::new(HashMap::new()),
            };
            ledger.record_deployment("COM3", "fw", "src", "/p", "env", None);
        }

        // Load from disk
        {
            let ledger = FirmwareLedger {
                path: ledger_path.clone(),
                entries: Mutex::new(FirmwareLedger::load_from_disk(&ledger_path)),
            };
            let entry = ledger.get_deployment("COM3").unwrap();
            assert_eq!(entry.firmware_hash, "fw");
        }
    }
}
