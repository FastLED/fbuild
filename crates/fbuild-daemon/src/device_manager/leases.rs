//! Lease mutations serialized by the inventory mutex.

use super::lease_store::StoredLease;
use super::{DeviceInventory, DeviceLease, DeviceLeaseError, DeviceManager, LeaseType};
use std::num::NonZeroU32;
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
        self.acquire_exclusive_with_ttl(port, client_id, description, track_serial, None)
    }

    /// Acquire an exclusive lease, optionally expiring it after a bounded duration.
    pub fn acquire_exclusive_with_ttl(
        &self,
        port: &str,
        client_id: &str,
        description: &str,
        track_serial: bool,
        ttl_seconds: Option<NonZeroU32>,
    ) -> Result<DeviceLease, DeviceLeaseError> {
        let mut inventory = self.inventory.lock().unwrap_or_else(|e| e.into_inner());
        let DeviceInventory {
            devices,
            lease_store,
        } = &mut *inventory;
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

        if let Some(existing) = state.active_exclusive_lease() {
            return Err(DeviceLeaseError::ExclusiveConflict {
                port: port.to_string(),
                device_id: state.device_id.clone(),
                description: state.description.clone(),
                holder: Box::new(existing.clone()),
            });
        }

        let acquired_at = Self::now_unix();
        let lease = DeviceLease {
            lease_id: Uuid::new_v4().to_string(),
            client_id: client_id.to_string(),
            lease_type: LeaseType::Exclusive,
            description: description.to_string(),
            acquired_at,
            track_serial,
            expires_at: ttl_seconds.map(|seconds| acquired_at + f64::from(seconds.get())),
        };

        if let Some(store) = lease_store {
            store.update(None, Some(StoredLease::new(state, lease.clone())))?;
        }
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
        let mut inventory = self.inventory.lock().unwrap_or_else(|e| e.into_inner());
        let devices = &mut inventory.devices;
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
            expires_at: None,
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
        let mut inventory = self.inventory.lock().unwrap_or_else(|e| e.into_inner());
        let DeviceInventory {
            devices,
            lease_store,
        } = &mut *inventory;
        let released_from_store = match lease_store {
            Some(store) => store
                .update(Some(lease_id), None)
                .map_err(|error| error.message())?,
            None => false,
        };
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
        if released_from_store {
            Ok(())
        } else {
            Err(format!("lease '{}' not found", lease_id))
        }
    }

    /// Release all leases for a device (by port).
    pub fn release_device_leases(&self, port: &str) -> Result<usize, String> {
        let mut inventory = self.inventory.lock().unwrap_or_else(|e| e.into_inner());
        let DeviceInventory {
            devices,
            lease_store,
        } = &mut *inventory;
        let state = devices
            .get_mut(port)
            .ok_or_else(|| format!("device '{}' not found", port))?;

        let mut count = 0;
        if let Some(lease) = &state.exclusive_lease {
            if let Some(store) = lease_store {
                store
                    .update(Some(&lease.lease_id), None)
                    .map_err(|error| error.message())?;
            }
            state.exclusive_lease = None;
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

        let mut inventory = self.inventory.lock().unwrap_or_else(|e| e.into_inner());
        let DeviceInventory {
            devices,
            lease_store,
        } = &mut *inventory;
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
        let preempted_client_id = state.active_exclusive_lease().map(|l| l.client_id.clone());

        // Log the preemption
        if let Some(existing) = state.active_exclusive_lease() {
            tracing::warn!(
                "preempting exclusive lease on '{}': holder='{}', new_client='{}', reason='{}'",
                port,
                existing.client_id,
                client_id,
                reason
            );
        }

        // Grant exclusive to new client
        let lease = DeviceLease {
            lease_id: Uuid::new_v4().to_string(),
            client_id: client_id.to_string(),
            lease_type: LeaseType::Exclusive,
            description: format!("preempted: {}", reason),
            acquired_at: Self::now_unix(),
            track_serial: false,
            expires_at: None,
        };

        if let Some(store) = lease_store {
            let old = state
                .exclusive_lease
                .as_ref()
                .map(|lease| lease.lease_id.as_str());
            store.update(old, Some(StoredLease::new(state, lease.clone())))?;
        }
        state.exclusive_lease = Some(lease.clone());
        state.monitor_leases.clear();
        Ok((lease, preempted_client_id))
    }
}
