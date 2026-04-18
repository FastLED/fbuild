//! In-memory device lease manager.
//!
//! Tracks connected serial devices and manages exclusive/monitor leases.
//! All locking is in-memory (no file-based locks per design rules).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Type of device lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeaseType {
    Exclusive,
    Monitor,
}

/// A lease held on a device.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceLease {
    pub lease_id: String,
    pub client_id: String,
    pub lease_type: LeaseType,
    pub description: String,
    pub acquired_at: f64,
}

/// In-memory record of the firmware image the daemon *last observed*
/// on a given port (either written by us, or confirmed by
/// `verify-flash` MD5 match). Used by the session-trusted verify-skip
/// path — see [`DeviceManager::trusted_firmware_hash`].
#[derive(Debug, Clone, Copy)]
pub struct TrustedFirmwareHash {
    /// SHA-256 of the (offset, size, bytes) tuples for all flashed
    /// regions. Stable across rebuilds that produce identical output.
    pub hash: [u8; 32],
    /// Instant when this hash was recorded. Compared against
    /// [`DeviceState::last_disconnect_at`] to invalidate trust on any
    /// device disconnect — if the user unplugged the board, something
    /// else may have flashed it before it came back.
    pub set_at: Instant,
}

/// Per-device tracked state.
#[derive(Debug, Clone)]
pub struct DeviceState {
    pub device_id: String,
    pub port: String,
    pub description: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub exclusive_lease: Option<DeviceLease>,
    pub monitor_leases: HashMap<String, DeviceLease>,
    pub last_seen_at: f64,
    pub is_connected: bool,
    /// Firmware image last seen on this device, or `None` if the
    /// daemon has not deployed/verified since startup. Cleared on
    /// disconnect through [`DeviceState::last_disconnect_at`].
    pub trusted_firmware: Option<TrustedFirmwareHash>,
    /// `Instant` of the most recent `true → false` transition on
    /// `is_connected`. Used to invalidate `trusted_firmware` if a
    /// disconnect happened after the trust was set. Populated at
    /// runtime, never serialized.
    pub last_disconnect_at: Option<Instant>,
}

impl DeviceState {
    pub fn is_available_for_exclusive(&self) -> bool {
        self.exclusive_lease.is_none()
    }

    pub fn monitor_count(&self) -> usize {
        self.monitor_leases.len()
    }
}

/// Thread-safe device manager.
pub struct DeviceManager {
    devices: Mutex<HashMap<String, DeviceState>>,
}

impl Default for DeviceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceManager {
    pub fn new() -> Self {
        Self {
            devices: Mutex::new(HashMap::new()),
        }
    }

    fn now_unix() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    }

    /// Refresh the device inventory from serial port enumeration.
    /// Preserves existing leases for devices that are still present.
    pub fn refresh_devices(&self) {
        let ports = match serialport::available_ports() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("failed to enumerate serial ports: {}", e);
                return;
            }
        };

        let mut devices = self.devices.lock().unwrap();
        let now = Self::now_unix();

        // Mark all devices as disconnected first. Track the previous
        // `is_connected` state so we can stamp `last_disconnect_at`
        // on the `true → false` edge *after* the re-enumeration pass
        // below re-flips surviving devices. The stamp lets
        // `trusted_firmware_hash` invalidate any trust that predates
        // a physical disconnect — see `TrustedFirmwareHash::set_at`.
        let mut was_connected: HashMap<String, bool> = HashMap::with_capacity(devices.len());
        for (key, state) in devices.iter_mut() {
            was_connected.insert(key.clone(), state.is_connected);
            state.is_connected = false;
        }

        // Update from discovered ports
        for port_info in ports {
            let (vid, pid, desc) = match &port_info.port_type {
                serialport::SerialPortType::UsbPort(usb) => (
                    Some(usb.vid),
                    Some(usb.pid),
                    usb.product
                        .clone()
                        .unwrap_or_else(|| "USB Serial Device".to_string()),
                ),
                serialport::SerialPortType::BluetoothPort => {
                    (None, None, "Bluetooth Serial".to_string())
                }
                serialport::SerialPortType::PciPort => (None, None, "PCI Serial".to_string()),
                serialport::SerialPortType::Unknown => (None, None, "Unknown".to_string()),
            };

            let device_id = vid
                .map(|v| format!("{:04x}:{:04x}", v, pid.unwrap_or(0)))
                .unwrap_or_else(|| port_info.port_name.clone());

            // Use port name as the unique key (device_id can have duplicates for same VID:PID)
            let key = port_info.port_name.clone();

            let entry = devices.entry(key).or_insert_with(|| DeviceState {
                device_id: device_id.clone(),
                port: port_info.port_name.clone(),
                description: desc.clone(),
                vid,
                pid,
                exclusive_lease: None,
                monitor_leases: HashMap::new(),
                last_seen_at: now,
                is_connected: true,
                trusted_firmware: None,
                last_disconnect_at: None,
            });

            entry.is_connected = true;
            entry.last_seen_at = now;
            entry.description = desc;
            entry.device_id = device_id;
            entry.vid = vid;
            entry.pid = pid;
        }

        // Stamp `last_disconnect_at` for every device that went from
        // `true → false` on this refresh (i.e. it was connected
        // before, but absent from this enumeration). Devices that
        // came back on this pass stay with `is_connected = true` and
        // don't get a fresh stamp.
        for (key, prev) in was_connected {
            if prev {
                if let Some(state) = devices.get_mut(&key) {
                    if !state.is_connected {
                        state.last_disconnect_at = Some(Instant::now());
                    }
                }
            }
        }
    }

    /// Get all devices.
    pub fn get_all_devices(&self) -> HashMap<String, DeviceState> {
        self.devices.lock().unwrap().clone()
    }

    /// Get status for a specific device (by port name).
    pub fn get_device_status(&self, port: &str) -> Option<DeviceState> {
        self.devices.lock().unwrap().get(port).cloned()
    }

    /// Acquire an exclusive lease on a device.
    pub fn acquire_exclusive(
        &self,
        port: &str,
        client_id: &str,
        description: &str,
    ) -> Result<DeviceLease, String> {
        let mut devices = self.devices.lock().unwrap();
        let state = devices
            .get_mut(port)
            .ok_or_else(|| format!("device '{}' not found", port))?;

        if !state.is_connected {
            return Err(format!("device '{}' is disconnected", port));
        }

        if let Some(ref existing) = state.exclusive_lease {
            return Err(format!(
                "device '{}' already has exclusive lease held by client '{}' ({})",
                port, existing.client_id, existing.description
            ));
        }

        let lease = DeviceLease {
            lease_id: Uuid::new_v4().to_string(),
            client_id: client_id.to_string(),
            lease_type: LeaseType::Exclusive,
            description: description.to_string(),
            acquired_at: Self::now_unix(),
        };

        state.exclusive_lease = Some(lease.clone());
        tracing::info!(
            "exclusive lease acquired on '{}' by client '{}'",
            port,
            client_id
        );
        Ok(lease)
    }

    /// Acquire a monitor lease on a device.
    pub fn acquire_monitor(
        &self,
        port: &str,
        client_id: &str,
        description: &str,
    ) -> Result<DeviceLease, String> {
        let mut devices = self.devices.lock().unwrap();
        let state = devices
            .get_mut(port)
            .ok_or_else(|| format!("device '{}' not found", port))?;

        if !state.is_connected {
            return Err(format!("device '{}' is disconnected", port));
        }

        let lease = DeviceLease {
            lease_id: Uuid::new_v4().to_string(),
            client_id: client_id.to_string(),
            lease_type: LeaseType::Monitor,
            description: description.to_string(),
            acquired_at: Self::now_unix(),
        };

        state
            .monitor_leases
            .insert(lease.lease_id.clone(), lease.clone());
        tracing::info!(
            "monitor lease acquired on '{}' by client '{}'",
            port,
            client_id
        );
        Ok(lease)
    }

    /// Release a lease by lease_id (searches all devices).
    pub fn release_lease(&self, lease_id: &str) -> Result<(), String> {
        let mut devices = self.devices.lock().unwrap();
        for state in devices.values_mut() {
            // Check exclusive
            if let Some(ref exc) = state.exclusive_lease {
                if exc.lease_id == lease_id {
                    let port = state.port.clone();
                    state.exclusive_lease = None;
                    tracing::info!("exclusive lease '{}' released on '{}'", lease_id, port);
                    return Ok(());
                }
            }
            // Check monitors
            if state.monitor_leases.remove(lease_id).is_some() {
                tracing::info!("monitor lease '{}' released on '{}'", lease_id, state.port);
                return Ok(());
            }
        }
        Err(format!("lease '{}' not found", lease_id))
    }

    /// Release all leases for a device (by port).
    pub fn release_device_leases(&self, port: &str) -> Result<usize, String> {
        let mut devices = self.devices.lock().unwrap();
        let state = devices
            .get_mut(port)
            .ok_or_else(|| format!("device '{}' not found", port))?;

        let mut count = 0;
        if state.exclusive_lease.take().is_some() {
            count += 1;
        }
        count += state.monitor_leases.len();
        state.monitor_leases.clear();
        tracing::info!("released {} lease(s) on '{}'", count, port);
        Ok(count)
    }

    /// Preempt a device: forcibly take it from the current holder.
    /// Returns `(new_lease, preempted_client_id)`.
    pub fn preempt_device(
        &self,
        port: &str,
        client_id: &str,
        reason: &str,
    ) -> Result<(DeviceLease, Option<String>), String> {
        if reason.is_empty() {
            return Err("preemption reason is required".to_string());
        }

        let mut devices = self.devices.lock().unwrap();
        let state = devices
            .get_mut(port)
            .ok_or_else(|| format!("device '{}' not found", port))?;

        if !state.is_connected {
            return Err(format!("device '{}' is disconnected", port));
        }

        // Capture preempted client before clearing
        let preempted_client_id = state.exclusive_lease.as_ref().map(|l| l.client_id.clone());

        // Log the preemption
        if let Some(ref existing) = state.exclusive_lease {
            tracing::warn!(
                "preempting exclusive lease on '{}': holder='{}', new_client='{}', reason='{}'",
                port,
                existing.client_id,
                client_id,
                reason
            );
        }

        // Clear all existing leases
        state.exclusive_lease = None;
        state.monitor_leases.clear();

        // Grant exclusive to new client
        let lease = DeviceLease {
            lease_id: Uuid::new_v4().to_string(),
            client_id: client_id.to_string(),
            lease_type: LeaseType::Exclusive,
            description: format!("preempted: {}", reason),
            acquired_at: Self::now_unix(),
        };

        state.exclusive_lease = Some(lease.clone());
        Ok((lease, preempted_client_id))
    }

    /// Return the currently-trusted firmware hash for `port`, if any.
    ///
    /// Trust is valid only if:
    ///  1. [`DeviceState::trusted_firmware`] is `Some`, AND
    ///  2. The port is currently connected, AND
    ///  3. No disconnect has been observed since the hash was recorded
    ///     (i.e. [`DeviceState::last_disconnect_at`] is either `None`
    ///     or older than `trusted_firmware.set_at`).
    ///
    /// Returns `None` in every other case, so the deploy handler falls
    /// back to the regular `verify-flash` path on any doubt.
    pub fn trusted_firmware_hash(&self, port: &str) -> Option<[u8; 32]> {
        let devices = self.devices.lock().unwrap();
        let state = devices.get(port)?;
        if !state.is_connected {
            return None;
        }
        let trusted = state.trusted_firmware.as_ref()?;
        if let Some(disc) = state.last_disconnect_at {
            if disc > trusted.set_at {
                return None;
            }
        }
        Some(trusted.hash)
    }

    /// Record a newly-observed firmware hash for `port`, stamped
    /// with `Instant::now()`. Called after a successful write-flash
    /// *or* a successful verify-flash match — in both cases the
    /// daemon knows exactly what the device holds right now.
    ///
    /// Silently no-ops if the port isn't in the enumeration cache
    /// yet: the hash will be recorded on the next deploy once
    /// `refresh_devices` has picked the port up.
    pub fn set_trusted_firmware_hash(&self, port: &str, hash: [u8; 32]) {
        let mut devices = self.devices.lock().unwrap();
        if let Some(state) = devices.get_mut(port) {
            state.trusted_firmware = Some(TrustedFirmwareHash {
                hash,
                set_at: Instant::now(),
            });
        }
    }

    /// Drop any recorded trust for `port`. Called when a deploy
    /// fails part-way through — partial writes mean we can't say
    /// what's on the chip anymore.
    pub fn clear_trusted_firmware_hash(&self, port: &str) {
        let mut devices = self.devices.lock().unwrap();
        if let Some(state) = devices.get_mut(port) {
            state.trusted_firmware = None;
        }
    }

    /// Remove stale disconnected devices that have no leases.
    pub fn cleanup_stale_devices(&self) -> usize {
        let mut devices = self.devices.lock().unwrap();
        let stale: Vec<String> = devices
            .iter()
            .filter(|(_, s)| {
                !s.is_connected && s.exclusive_lease.is_none() && s.monitor_leases.is_empty()
            })
            .map(|(k, _)| k.clone())
            .collect();
        let count = stale.len();
        for key in stale {
            devices.remove(&key);
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager_with_device(port: &str) -> DeviceManager {
        let mgr = DeviceManager::new();
        {
            let mut devices = mgr.devices.lock().unwrap();
            devices.insert(
                port.to_string(),
                DeviceState {
                    device_id: "1234:5678".to_string(),
                    port: port.to_string(),
                    description: "Test Device".to_string(),
                    vid: Some(0x1234),
                    pid: Some(0x5678),
                    exclusive_lease: None,
                    monitor_leases: HashMap::new(),
                    last_seen_at: DeviceManager::now_unix(),
                    is_connected: true,
                    trusted_firmware: None,
                    last_disconnect_at: None,
                },
            );
        }
        mgr
    }

    #[test]
    fn new_manager_has_no_devices() {
        let mgr = DeviceManager::new();
        assert!(mgr.get_all_devices().is_empty());
    }

    #[test]
    fn acquire_exclusive_succeeds() {
        let mgr = make_manager_with_device("COM3");
        let lease = mgr
            .acquire_exclusive("COM3", "client-1", "testing")
            .unwrap();
        assert_eq!(lease.lease_type, LeaseType::Exclusive);
        assert_eq!(lease.client_id, "client-1");
    }

    #[test]
    fn acquire_exclusive_twice_fails() {
        let mgr = make_manager_with_device("COM3");
        mgr.acquire_exclusive("COM3", "client-1", "first").unwrap();
        let result = mgr.acquire_exclusive("COM3", "client-2", "second");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already has exclusive lease"));
    }

    #[test]
    fn acquire_monitor_succeeds() {
        let mgr = make_manager_with_device("COM3");
        let lease = mgr
            .acquire_monitor("COM3", "client-1", "monitoring")
            .unwrap();
        assert_eq!(lease.lease_type, LeaseType::Monitor);
    }

    #[test]
    fn multiple_monitor_leases_allowed() {
        let mgr = make_manager_with_device("COM3");
        mgr.acquire_monitor("COM3", "client-1", "m1").unwrap();
        mgr.acquire_monitor("COM3", "client-2", "m2").unwrap();
        let state = mgr.get_device_status("COM3").unwrap();
        assert_eq!(state.monitor_count(), 2);
    }

    #[test]
    fn release_lease_by_id() {
        let mgr = make_manager_with_device("COM3");
        let lease = mgr.acquire_exclusive("COM3", "client-1", "test").unwrap();
        mgr.release_lease(&lease.lease_id).unwrap();
        let state = mgr.get_device_status("COM3").unwrap();
        assert!(state.exclusive_lease.is_none());
    }

    #[test]
    fn release_nonexistent_lease_fails() {
        let mgr = make_manager_with_device("COM3");
        assert!(mgr.release_lease("nonexistent").is_err());
    }

    #[test]
    fn release_device_leases_clears_all() {
        let mgr = make_manager_with_device("COM3");
        mgr.acquire_exclusive("COM3", "c1", "exc").unwrap();
        mgr.acquire_monitor("COM3", "c2", "mon").unwrap();
        let count = mgr.release_device_leases("COM3").unwrap();
        assert_eq!(count, 2); // 1 exclusive + 1 monitor
        let state = mgr.get_device_status("COM3").unwrap();
        assert!(state.exclusive_lease.is_none());
        assert_eq!(state.monitor_count(), 0);
    }

    #[test]
    fn preempt_requires_reason() {
        let mgr = make_manager_with_device("COM3");
        mgr.acquire_exclusive("COM3", "c1", "holder").unwrap();
        let result = mgr.preempt_device("COM3", "c2", "");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("reason is required"));
    }

    #[test]
    fn preempt_replaces_holder() {
        let mgr = make_manager_with_device("COM3");
        mgr.acquire_exclusive("COM3", "c1", "original").unwrap();
        let (lease, preempted) = mgr.preempt_device("COM3", "c2", "urgent deploy").unwrap();
        assert_eq!(lease.client_id, "c2");
        assert_eq!(lease.lease_type, LeaseType::Exclusive);
        assert_eq!(preempted.as_deref(), Some("c1"));
        // Old holder should be gone
        let state = mgr.get_device_status("COM3").unwrap();
        assert_eq!(state.exclusive_lease.as_ref().unwrap().client_id, "c2");
    }

    #[test]
    fn device_not_found_errors() {
        let mgr = DeviceManager::new();
        assert!(mgr.acquire_exclusive("COM99", "c1", "x").is_err());
        assert!(mgr.acquire_monitor("COM99", "c1", "x").is_err());
        assert!(mgr.preempt_device("COM99", "c1", "reason").is_err());
    }

    #[test]
    fn disconnected_device_rejects_leases() {
        let mgr = make_manager_with_device("COM3");
        {
            let mut devices = mgr.devices.lock().unwrap();
            devices.get_mut("COM3").unwrap().is_connected = false;
        }
        assert!(mgr.acquire_exclusive("COM3", "c1", "x").is_err());
        assert!(mgr.acquire_monitor("COM3", "c1", "x").is_err());
        assert!(mgr.preempt_device("COM3", "c1", "reason").is_err());
    }

    #[test]
    fn cleanup_removes_disconnected_unlocked() {
        let mgr = make_manager_with_device("COM3");
        {
            let mut devices = mgr.devices.lock().unwrap();
            devices.get_mut("COM3").unwrap().is_connected = false;
        }
        assert_eq!(mgr.cleanup_stale_devices(), 1);
        assert!(mgr.get_all_devices().is_empty());
    }

    #[test]
    fn trusted_hash_round_trip() {
        let mgr = make_manager_with_device("COM3");
        assert_eq!(mgr.trusted_firmware_hash("COM3"), None);
        let h = [7u8; 32];
        mgr.set_trusted_firmware_hash("COM3", h);
        assert_eq!(mgr.trusted_firmware_hash("COM3"), Some(h));
    }

    #[test]
    fn trusted_hash_cleared_on_demand() {
        let mgr = make_manager_with_device("COM3");
        mgr.set_trusted_firmware_hash("COM3", [1u8; 32]);
        mgr.clear_trusted_firmware_hash("COM3");
        assert_eq!(mgr.trusted_firmware_hash("COM3"), None);
    }

    /// Unknown port is never trusted — no panic, no fabrication of
    /// device state. Regression guard: the deploy handler calls this
    /// with a user-supplied port string that may not be in the
    /// daemon's enumeration cache yet.
    #[test]
    fn trusted_hash_unknown_port_is_none() {
        let mgr = DeviceManager::new();
        assert_eq!(mgr.trusted_firmware_hash("COM99"), None);
    }

    /// A disconnected device must never be trusted, even if the hash
    /// was previously recorded. Re-enumeration can come back with a
    /// physically different board on the same port name — trust
    /// across that boundary is unsafe.
    #[test]
    fn trusted_hash_invalid_after_disconnect() {
        let mgr = make_manager_with_device("COM3");
        mgr.set_trusted_firmware_hash("COM3", [9u8; 32]);
        // Simulate a disconnect that happened *after* the trust was
        // recorded — exactly the condition
        // `trusted_firmware_hash` must treat as "unsafe to trust".
        {
            let mut devices = mgr.devices.lock().unwrap();
            let state = devices.get_mut("COM3").unwrap();
            state.is_connected = false;
            state.last_disconnect_at = Some(Instant::now());
        }
        assert_eq!(mgr.trusted_firmware_hash("COM3"), None);
    }

    /// A stale disconnect stamp from *before* the trust was set
    /// (e.g. device was unplugged earlier in the session but the
    /// user reconnected and we re-trusted) does NOT invalidate the
    /// fresh trust. Ordering is what matters: `disconnect > set` ⇒
    /// untrusted; `set > disconnect` ⇒ trusted.
    #[test]
    fn trusted_hash_survives_older_disconnect_stamp() {
        let mgr = make_manager_with_device("COM3");
        // Plant an old disconnect stamp first, then re-set trust
        // later (what happens on a re-enumerate + fresh deploy).
        {
            let mut devices = mgr.devices.lock().unwrap();
            let state = devices.get_mut("COM3").unwrap();
            state.last_disconnect_at = Some(Instant::now());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        mgr.set_trusted_firmware_hash("COM3", [3u8; 32]);
        assert_eq!(mgr.trusted_firmware_hash("COM3"), Some([3u8; 32]));
    }

    #[test]
    fn cleanup_preserves_leased_disconnected() {
        let mgr = make_manager_with_device("COM3");
        mgr.acquire_exclusive("COM3", "c1", "deploy").unwrap();
        {
            let mut devices = mgr.devices.lock().unwrap();
            devices.get_mut("COM3").unwrap().is_connected = false;
        }
        assert_eq!(mgr.cleanup_stale_devices(), 0);
        assert_eq!(mgr.get_all_devices().len(), 1);
    }
}
