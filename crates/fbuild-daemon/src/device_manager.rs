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
    pub track_serial: bool,
}

/// Structured error for lease acquisition failures.
#[derive(Debug, Clone)]
pub enum DeviceLeaseError {
    NotFound {
        port: String,
    },
    Disconnected {
        port: String,
    },
    ExclusiveConflict {
        port: String,
        device_id: String,
        description: String,
        holder: Box<DeviceLease>,
    },
    InvalidLeaseType {
        lease_type: String,
    },
    InvalidPreemption {
        message: String,
    },
}

impl DeviceLeaseError {
    pub fn message(&self) -> String {
        match self {
            Self::NotFound { port } => format!("device '{}' not found", port),
            Self::Disconnected { port } => format!("device '{}' is disconnected", port),
            Self::ExclusiveConflict { port, holder, .. } => format!(
                "device '{}' already has exclusive lease held by client '{}' ({})",
                port, holder.client_id, holder.description
            ),
            Self::InvalidLeaseType { lease_type } => format!(
                "invalid lease_type '{}', must be 'exclusive' or 'monitor'",
                lease_type
            ),
            Self::InvalidPreemption { message } => message.clone(),
        }
    }
}

impl std::fmt::Display for DeviceLeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for DeviceLeaseError {}

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
    /// Human-readable USB vendor name, resolved from `vid` via
    /// [`fbuild_core::usb::resolve`]. `None` only when the device has no
    /// `vid` (e.g. bluetooth/PCI serial). Tier-1/2/3 fallbacks guarantee a
    /// string when a `vid` exists. See [`crate::device_manager`].
    pub vendor_name: Option<String>,
    /// Human-readable USB product name (same provenance as `vendor_name`).
    pub product_name: Option<String>,
    /// Whether the OS classified this USB serial endpoint as CDC-ACM.
    /// `Some(false)` means a USB-serial bridge driver; `None` means
    /// non-USB or unknown on this platform.
    pub is_cdc: Option<bool>,
    pub serial_number: Option<String>,
    pub previous_port: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePortMove {
    pub previous_port: String,
    pub port: String,
    pub serial_number: Option<String>,
}

impl DeviceState {
    pub fn is_available_for_exclusive(&self) -> bool {
        self.exclusive_lease.is_none()
    }

    pub fn monitor_count(&self) -> usize {
        self.monitor_leases.len()
    }

    pub fn has_tracked_serial_lease(&self) -> bool {
        self.exclusive_lease
            .as_ref()
            .map(|lease| lease.track_serial)
            .unwrap_or(false)
            || self.monitor_leases.values().any(|lease| lease.track_serial)
    }
}

#[derive(Debug, Clone)]
struct DiscoveredDevice {
    port: String,
    device_id: String,
    description: String,
    vid: Option<u16>,
    pid: Option<u16>,
    vendor_name: Option<String>,
    product_name: Option<String>,
    is_cdc: Option<bool>,
    serial_number: Option<String>,
}

/// Thread-safe device manager.
pub struct DeviceManager {
    devices: Mutex<HashMap<String, DeviceState>>,
    /// `Instant` of the most recent successful [`Self::refresh_devices`]
    /// call. Used by [`Self::refresh_devices_if_stale`] to skip the
    /// OS-level port enumeration (~20–30 ms on Windows) when the
    /// enumeration cache is still fresh — the dominant cost on
    /// back-to-back warm deploys.
    last_refresh_at: Mutex<Option<Instant>>,
    recent_port_moves: Mutex<Vec<DevicePortMove>>,
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
            last_refresh_at: Mutex::new(None),
            recent_port_moves: Mutex::new(Vec::new()),
        }
    }

    fn now_unix() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    }

    /// Refresh the device inventory only if the last refresh is older
    /// than `max_age`. Returns `true` if a refresh actually ran.
    ///
    /// Called by the deploy handler with a small `max_age` (e.g. 2 s)
    /// so back-to-back warm deploys don't re-pay the OS port
    /// enumeration cost (~20–30 ms on Windows). The trust-cache
    /// invalidation logic still requires a refresh to have happened
    /// *recently enough* — we just don't need one on every deploy.
    pub fn refresh_devices_if_stale(&self, max_age: std::time::Duration) -> bool {
        {
            let last = self
                .last_refresh_at
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(t) = *last {
                if t.elapsed() < max_age {
                    return false;
                }
            }
        }
        self.refresh_devices();
        true
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

        let discovered: Vec<DiscoveredDevice> = ports
            .into_iter()
            .map(|port_info| {
                let (vid, pid, fallback_desc) = match &port_info.port_type {
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
                let is_cdc = match &port_info.port_type {
                    serialport::SerialPortType::UsbPort(_) => detect_is_cdc(&port_info.port_name),
                    _ => None,
                };
                // Resolve VID:PID → pretty (vendor, product) via the verified
                // FastLED/boards runtime cache. The resolver-derived
                // description wins over the (often blank or generic) string
                // returned by the OS-level enumerator. Bluetooth / PCI /
                // unknown ports keep their static fallback descriptor.
                let (vendor_name, product_name, description) = match (vid, pid) {
                    (Some(v), Some(p)) => {
                        let info = fbuild_core::usb::resolve(v, p);
                        let desc = format!("{} {}", info.vendor, info.product);
                        (Some(info.vendor), Some(info.product), desc)
                    }
                    _ => (None, None, fallback_desc),
                };
                let serial_number = match &port_info.port_type {
                    serialport::SerialPortType::UsbPort(usb) => usb.serial_number.clone(),
                    _ => None,
                };
                let device_id = vid
                    .map(|v| format!("{:04x}:{:04x}", v, pid.unwrap_or(0)))
                    .unwrap_or_else(|| port_info.port_name.clone());

                DiscoveredDevice {
                    port: port_info.port_name,
                    device_id,
                    description,
                    vid,
                    pid,
                    vendor_name,
                    product_name,
                    is_cdc,
                    serial_number,
                }
            })
            .collect();

        self.refresh_from_discovered(discovered);
        *self
            .last_refresh_at
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    fn refresh_from_discovered(&self, discovered: Vec<DiscoveredDevice>) {
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
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
        for device in discovered {
            let key = device.port.clone();
            if !devices.contains_key(&key) {
                if let Some(serial) = device.serial_number.as_deref() {
                    let moved_from = devices.iter().find_map(|(old_port, state)| {
                        (state.serial_number.as_deref() == Some(serial)
                            && !state.is_connected
                            && state.has_tracked_serial_lease())
                        .then(|| old_port.clone())
                    });
                    if let Some(old_port) = moved_from {
                        if let Some(mut state) = devices.remove(&old_port) {
                            let serial_number = device.serial_number.clone();
                            state.previous_port = Some(old_port);
                            state.port = key.clone();
                            state.is_connected = true;
                            state.last_seen_at = now;
                            state.description = device.description;
                            state.device_id = device.device_id;
                            state.vid = device.vid;
                            state.pid = device.pid;
                            state.vendor_name = device.vendor_name;
                            state.product_name = device.product_name;
                            state.is_cdc = device.is_cdc;
                            state.serial_number = device.serial_number;
                            if let Some(previous_port) = state.previous_port.clone() {
                                self.recent_port_moves
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .push(DevicePortMove {
                                        previous_port,
                                        port: key.clone(),
                                        serial_number,
                                    });
                            }
                            devices.insert(key, state);
                            continue;
                        }
                    }
                }
            }

            let entry = devices.entry(key).or_insert_with(|| DeviceState {
                device_id: device.device_id.clone(),
                port: device.port.clone(),
                description: device.description.clone(),
                vid: device.vid,
                pid: device.pid,
                vendor_name: device.vendor_name.clone(),
                product_name: device.product_name.clone(),
                is_cdc: device.is_cdc,
                serial_number: device.serial_number.clone(),
                previous_port: None,
                exclusive_lease: None,
                monitor_leases: HashMap::new(),
                last_seen_at: now,
                is_connected: true,
                trusted_firmware: None,
                last_disconnect_at: None,
            });

            entry.is_connected = true;
            entry.last_seen_at = now;
            entry.description = device.description;
            entry.device_id = device.device_id;
            entry.vid = device.vid;
            entry.pid = device.pid;
            entry.vendor_name = device.vendor_name;
            entry.product_name = device.product_name;
            entry.is_cdc = device.is_cdc;
            entry.serial_number = device.serial_number;
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
        self.devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn take_recent_port_moves(&self) -> Vec<DevicePortMove> {
        let mut moves = self
            .recent_port_moves
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *moves)
    }

    /// Get status for a specific device (by port name).
    pub fn get_device_status(&self, port: &str) -> Option<DeviceState> {
        self.devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(port)
            .cloned()
    }

    /// Acquire an exclusive lease on a device.
    pub fn acquire_exclusive(
        &self,
        port: &str,
        client_id: &str,
        description: &str,
        track_serial: bool,
    ) -> Result<DeviceLease, DeviceLeaseError> {
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        let state = devices
            .get_mut(port)
            .ok_or_else(|| DeviceLeaseError::NotFound {
                port: port.to_string(),
            })?;

        if !state.is_connected {
            return Err(DeviceLeaseError::Disconnected {
                port: port.to_string(),
            });
        }

        if let Some(ref existing) = state.exclusive_lease {
            return Err(DeviceLeaseError::ExclusiveConflict {
                port: port.to_string(),
                device_id: state.device_id.clone(),
                description: state.description.clone(),
                holder: Box::new(existing.clone()),
            });
        }

        let lease = DeviceLease {
            lease_id: Uuid::new_v4().to_string(),
            client_id: client_id.to_string(),
            lease_type: LeaseType::Exclusive,
            description: description.to_string(),
            acquired_at: Self::now_unix(),
            track_serial,
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
        track_serial: bool,
    ) -> Result<DeviceLease, DeviceLeaseError> {
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        let state = devices
            .get_mut(port)
            .ok_or_else(|| DeviceLeaseError::NotFound {
                port: port.to_string(),
            })?;

        if !state.is_connected {
            return Err(DeviceLeaseError::Disconnected {
                port: port.to_string(),
            });
        }

        let lease = DeviceLease {
            lease_id: Uuid::new_v4().to_string(),
            client_id: client_id.to_string(),
            lease_type: LeaseType::Monitor,
            description: description.to_string(),
            acquired_at: Self::now_unix(),
            track_serial,
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
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
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
    ) -> Result<(DeviceLease, Option<String>), DeviceLeaseError> {
        if reason.is_empty() {
            return Err(DeviceLeaseError::InvalidPreemption {
                message: "preemption reason is required".to_string(),
            });
        }

        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        let state = devices
            .get_mut(port)
            .ok_or_else(|| DeviceLeaseError::NotFound {
                port: port.to_string(),
            })?;

        if !state.is_connected {
            return Err(DeviceLeaseError::Disconnected {
                port: port.to_string(),
            });
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
            track_serial: false,
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
        let devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = devices.get_mut(port) {
            state.trusted_firmware = None;
        }
    }

    /// Remove stale disconnected devices that have no leases.
    pub fn cleanup_stale_devices(&self) -> usize {
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
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

    #[cfg(test)]
    pub(crate) fn insert_test_device(&self, port: &str) {
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        devices.insert(
            port.to_string(),
            DeviceState {
                device_id: "1234:5678".to_string(),
                port: port.to_string(),
                description: "Test Device".to_string(),
                vid: Some(0x1234),
                pid: Some(0x5678),
                vendor_name: Some("Test Vendor".to_string()),
                product_name: Some("Test Device".to_string()),
                is_cdc: None,
                serial_number: Some("TEST-SERIAL".to_string()),
                previous_port: None,
                exclusive_lease: None,
                monitor_leases: HashMap::new(),
                last_seen_at: Self::now_unix(),
                is_connected: true,
                trusted_firmware: None,
                last_disconnect_at: None,
            },
        );
    }
}

fn detect_is_cdc(port_name: &str) -> Option<bool> {
    match fbuild_serial::port_class::detect_port_kernel_class(port_name)? {
        fbuild_serial::port_class::PortKernelClass::CdcAcm => Some(true),
        fbuild_serial::port_class::PortKernelClass::UsbSerialBridge => Some(false),
    }
}

#[cfg(test)]
mod tests;
