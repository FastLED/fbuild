//! Exclusive reservations written by the daemon that owns this cache root.

use super::{DeviceLease, DeviceLeaseError, DevicePortMove, DeviceState, LeaseType};
use fbuild_core::path::NormalizedPath;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::Path;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct StoredLease {
    port: String,
    #[serde(default)]
    previous_port: Option<String>,
    device_id: String,
    serial_number: Option<String>,
    lease: DeviceLease,
}

impl StoredLease {
    pub(super) fn new(state: &DeviceState, lease: DeviceLease) -> Self {
        Self {
            port: state.port.clone(),
            previous_port: state.previous_port.clone(),
            device_id: state.device_id.clone(),
            serial_number: state.serial_number.clone(),
            lease,
        }
    }

    fn matches(&self, state: &DeviceState) -> bool {
        state.is_connected
            && state.serial_number == self.serial_number
            && ((self.lease.track_serial && self.serial_number.is_some())
                || state.device_id == self.device_id)
    }

    fn renumbered_target<'a>(
        &self,
        devices: &'a HashMap<String, DeviceState>,
    ) -> Option<&'a DeviceState> {
        if !self.lease.track_serial || self.serial_number.is_none() {
            return None;
        }
        let mut candidates = devices.values().filter(|state| self.matches(state));
        let candidate = candidates.next()?;
        candidates.next().is_none().then_some(candidate)
    }

    fn attach(
        &self,
        devices: &mut HashMap<String, DeviceState>,
        port: &str,
        old_port: Option<&str>,
    ) -> Option<DevicePortMove> {
        if !devices.get(port)?.is_available_for_exclusive() {
            return None;
        }
        if let Some(old) = old_port.and_then(|port| devices.get_mut(port)) {
            if old
                .active_exclusive_lease()
                .is_some_and(|lease| lease.lease_id == self.lease.lease_id)
            {
                old.exclusive_lease = None;
            }
        }
        let state = devices.get_mut(port)?;
        state.exclusive_lease = Some(self.lease.clone());
        if self.port == port {
            state.previous_port.clone_from(&self.previous_port);
            return None;
        }
        state.previous_port = Some(self.port.clone());
        Some(DevicePortMove {
            previous_port: self.port.clone(),
            port: port.to_string(),
            serial_number: state.serial_number.clone(),
        })
    }
}

pub(super) struct LeaseStore {
    path: NormalizedPath,
    records: HashMap<String, StoredLease>,
}

impl LeaseStore {
    pub(super) async fn open(path: &Path) -> io::Result<Self> {
        let mut records = HashMap::new();
        match fbuild_core::fs::read(path).await {
            Ok(data) => {
                for record in serde_json::from_slice::<Vec<StoredLease>>(&data)? {
                    if record.lease.lease_type != LeaseType::Exclusive
                        || records
                            .insert(record.lease.lease_id.clone(), record)
                            .is_some()
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "invalid exclusive lease snapshot",
                        ));
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        records.retain(|_, record| record.lease.is_active());
        Ok(Self {
            path: NormalizedPath::new(path),
            records,
        })
    }

    pub(super) fn has_active_leases(&self) -> bool {
        self.records.values().any(|record| record.lease.is_active())
    }

    pub(super) fn holder_at_last_known_port(&self, port: &str) -> Option<DeviceLease> {
        self.records
            .values()
            .find(|record| record.port == port && record.lease.is_active())
            .map(|record| record.lease.clone())
    }

    /// Associate persisted leases with currently discovered devices. An
    /// unplugged board stays in the snapshot, where it can be released by ID.
    pub(super) fn restore(
        &mut self,
        devices: &mut HashMap<String, DeviceState>,
    ) -> Vec<DevicePortMove> {
        let mut moves = Vec::new();
        let mut pending = Vec::new();
        for record in self
            .records
            .values()
            .filter(|record| record.lease.is_active())
        {
            let attached = devices.values_mut().find(|state| {
                state
                    .active_exclusive_lease()
                    .is_some_and(|lease| lease.lease_id == record.lease.lease_id)
            });
            if attached.as_ref().is_some_and(|state| record.matches(state)) {
                continue;
            }
            // The old port can be reused by a different device. Remove the
            // binding from that port but keep the reservation in the snapshot.
            let old_port = attached.map(|state| {
                if state.is_connected {
                    state.exclusive_lease = None;
                }
                state.port.clone()
            });
            if devices
                .get(&record.port)
                .is_some_and(|state| record.matches(state))
            {
                let _ = record.attach(devices, &record.port, old_port.as_deref());
            } else {
                pending.push((record, old_port));
            }
        }

        // Resolve exact port matches above before any off-port lookup. Two
        // pending leases for a duplicate serial cannot claim a single board.
        for (record, old_port) in &pending {
            if pending.iter().any(|(other, _)| {
                other.lease.lease_id != record.lease.lease_id
                    && other.lease.track_serial
                    && other.serial_number == record.serial_number
            }) {
                continue;
            }
            let Some(port) = record
                .renumbered_target(devices)
                .map(|state| state.port.clone())
            else {
                continue;
            };
            if let Some(movement) = record.attach(devices, &port, old_port.as_deref()) {
                moves.push(movement);
            }
        }

        if let Err(error) = self.save_port_bindings(devices) {
            tracing::warn!(%error, "failed to persist device lease port changes");
        }
        moves
    }

    fn save_port_bindings(
        &mut self,
        devices: &HashMap<String, DeviceState>,
    ) -> Result<(), DeviceLeaseError> {
        let moved = devices
            .values()
            .filter_map(|state| {
                let lease = state.active_exclusive_lease()?;
                let record = self.records.get(&lease.lease_id)?;
                (state.is_connected && state.port != record.port).then(|| {
                    (
                        lease.lease_id.clone(),
                        StoredLease::new(state, lease.clone()),
                    )
                })
            })
            .collect::<Vec<_>>();
        if !moved.is_empty() {
            let mut next = self.records.clone();
            next.extend(moved);
            self.save(&next)?;
            self.records = next;
        }
        Ok(())
    }

    /// Commit the proposed change before publishing it to in-memory readers.
    /// The caller holds the inventory mutex across both steps.
    pub(super) fn update(
        &mut self,
        remove: Option<&str>,
        insert: Option<StoredLease>,
    ) -> Result<bool, DeviceLeaseError> {
        let removed = remove.is_some_and(|id| self.records.contains_key(id));
        if insert.is_none() && !removed {
            return Ok(false);
        }
        let mut next = self.records.clone();
        if let Some(id) = remove {
            next.remove(id);
        }
        next.retain(|_, record| record.lease.is_active());
        if let Some(record) = insert {
            next.insert(record.lease.lease_id.clone(), record);
        }
        self.save(&next)?;
        self.records = next;
        Ok(removed)
    }

    fn save(&self, records: &HashMap<String, StoredLease>) -> Result<(), DeviceLeaseError> {
        let write = || -> io::Result<()> {
            let data = serde_json::to_vec(&records.values().collect::<Vec<_>>())?;
            fbuild_core::fs::write_atomic_sync(&self.path, data)
        };
        write().map_err(|error| DeviceLeaseError::Persistence {
            message: format!(
                "cannot save exclusive device leases to {}: {error}",
                self.path
            ),
        })
    }
}
