//! Lease mutations serialized by the inventory mutex.

use super::{DeviceLease, DeviceLeaseError, DeviceManager, LeaseType};
use uuid::Uuid;

impl DeviceManager {
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
}
