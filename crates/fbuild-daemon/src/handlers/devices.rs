//! Device discovery and management handlers.

use crate::context::DaemonContext;
use crate::device_manager::{DeviceLease, DeviceLeaseError, DeviceState, LeaseType};
use crate::models::{
    DeviceInfo, DeviceLeaseConflict, DeviceLeaseInfo, DeviceLeaseRequest, DeviceLeaseResponse,
    DeviceListResponse, DevicePreemptRequest, DevicePreemptResponse, DeviceReleaseRequest,
    DeviceReleaseResponse, DeviceStatusResponse,
};
use axum::extract::{Path, State};
use axum::Json;
use std::sync::Arc;
use uuid::Uuid;

/// POST /api/devices/list
pub async fn list_devices(state: State<Arc<DaemonContext>>) -> Json<DeviceListResponse> {
    // Refresh device inventory
    state.device_manager.refresh_devices();

    let all = state.device_manager.get_all_devices();
    let devices = all.values().map(device_info).collect();

    Json(DeviceListResponse {
        success: true,
        devices,
    })
}

/// GET /api/devices/{port}/status
pub async fn device_status(
    state: State<Arc<DaemonContext>>,
    Path(port): Path<String>,
) -> Json<DeviceStatusResponse> {
    // Refresh to get latest state
    state.device_manager.refresh_devices();

    match state.device_manager.get_device_status(&port) {
        Some(ds) => Json(device_status_response(ds)),
        None => Json(DeviceStatusResponse {
            success: false,
            port: port.clone(),
            device_id: String::new(),
            description: format!("device '{}' not found", port),
            is_connected: false,
            available_for_exclusive: false,
            exclusive_holder: None,
            exclusive_lease: None,
            monitor_count: 0,
            monitor_leases: vec![],
        }),
    }
}

fn device_info(state: &DeviceState) -> DeviceInfo {
    DeviceInfo {
        port: state.port.clone(),
        device_id: Some(state.device_id.clone()),
        vid: state.vid,
        pid: state.pid,
        description: state.description.clone(),
        available_for_exclusive: state.is_available_for_exclusive(),
        exclusive_lease: state.exclusive_lease.as_ref().map(lease_info),
        monitor_count: state.monitor_count(),
    }
}

fn device_status_response(state: DeviceState) -> DeviceStatusResponse {
    let available = state.is_available_for_exclusive();
    let holder = state.exclusive_lease.as_ref().map(|l| l.client_id.clone());
    let monitors = state.monitor_count();
    let exclusive_lease = state.exclusive_lease.as_ref().map(lease_info);
    let monitor_leases = state.monitor_leases.values().map(lease_info).collect();
    DeviceStatusResponse {
        success: true,
        port: state.port,
        device_id: state.device_id,
        description: state.description,
        is_connected: state.is_connected,
        available_for_exclusive: available,
        exclusive_holder: holder,
        exclusive_lease,
        monitor_count: monitors,
        monitor_leases,
    }
}

/// POST /api/devices/{port}/lease
pub async fn device_lease(
    state: State<Arc<DaemonContext>>,
    Path(port): Path<String>,
    Json(req): Json<DeviceLeaseRequest>,
) -> Json<DeviceLeaseResponse> {
    let client_id = req.client_id.unwrap_or_else(|| Uuid::new_v4().to_string());

    // Ensure devices are refreshed
    state.device_manager.refresh_devices();

    let result = match req.lease_type.as_str() {
        "exclusive" => state
            .device_manager
            .acquire_exclusive(&port, &client_id, &req.description),
        "monitor" => state
            .device_manager
            .acquire_monitor(&port, &client_id, &req.description),
        other => Err(DeviceLeaseError::InvalidLeaseType {
            lease_type: other.to_string(),
        }),
    };

    match result {
        Ok(lease) => Json(DeviceLeaseResponse {
            success: true,
            lease_id: Some(lease.lease_id),
            lease_type: Some(match lease.lease_type {
                LeaseType::Exclusive => "exclusive".to_string(),
                LeaseType::Monitor => "monitor".to_string(),
            }),
            message: format!("lease acquired on '{}'", port),
            conflict: None,
        }),
        Err(err) => Json(DeviceLeaseResponse {
            success: false,
            lease_id: None,
            lease_type: None,
            message: err.message(),
            conflict: lease_conflict(&err),
        }),
    }
}

/// POST /api/devices/{port}/release
pub async fn device_release(
    state: State<Arc<DaemonContext>>,
    Path(port): Path<String>,
    Json(req): Json<DeviceReleaseRequest>,
) -> Json<DeviceReleaseResponse> {
    let result = if let Some(lease_id) = req.lease_id {
        state
            .device_manager
            .release_lease(&lease_id)
            .map(|()| 1usize)
    } else {
        state.device_manager.release_device_leases(&port)
    };

    match result {
        Ok(count) => Json(DeviceReleaseResponse {
            success: true,
            released_count: count,
            message: format!("released {} lease(s) on '{}'", count, port),
        }),
        Err(msg) => Json(DeviceReleaseResponse {
            success: false,
            released_count: 0,
            message: msg,
        }),
    }
}

/// POST /api/devices/{port}/preempt
pub async fn device_preempt(
    state: State<Arc<DaemonContext>>,
    Path(port): Path<String>,
    Json(req): Json<DevicePreemptRequest>,
) -> Json<DevicePreemptResponse> {
    let client_id = req.client_id.unwrap_or_else(|| Uuid::new_v4().to_string());

    // Refresh first
    state.device_manager.refresh_devices();

    match state
        .device_manager
        .preempt_device(&port, &client_id, &req.reason)
    {
        Ok((lease, preempted_client_id)) => Json(DevicePreemptResponse {
            success: true,
            lease_id: Some(lease.lease_id),
            preempted_client_id,
            message: format!("device '{}' preempted: {}", port, req.reason),
        }),
        Err(msg) => Json(DevicePreemptResponse {
            success: false,
            lease_id: None,
            preempted_client_id: None,
            message: msg.message(),
        }),
    }
}

fn lease_info(lease: &DeviceLease) -> DeviceLeaseInfo {
    DeviceLeaseInfo {
        lease_id: lease.lease_id.clone(),
        client_id: lease.client_id.clone(),
        lease_type: match lease.lease_type {
            LeaseType::Exclusive => "exclusive".to_string(),
            LeaseType::Monitor => "monitor".to_string(),
        },
        description: lease.description.clone(),
        acquired_at: lease.acquired_at,
    }
}

fn lease_conflict(err: &DeviceLeaseError) -> Option<DeviceLeaseConflict> {
    match err {
        DeviceLeaseError::ExclusiveConflict {
            port,
            device_id,
            description,
            holder,
        } => Some(DeviceLeaseConflict {
            port: port.clone(),
            device_id: device_id.clone(),
            description: description.clone(),
            holder: lease_info(holder),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_manager::DeviceManager;

    #[test]
    fn device_status_response_includes_full_lease_attribution() {
        let manager = DeviceManager::new();
        manager.insert_test_device("COM3");
        manager
            .acquire_exclusive("COM3", "pid 100 alice", "deploy")
            .unwrap();
        manager
            .acquire_monitor("COM3", "pid 200 bob", "monitor")
            .unwrap();

        let state = manager.get_device_status("COM3").unwrap();
        let response = device_status_response(state);

        assert!(!response.available_for_exclusive);
        let exclusive = response.exclusive_lease.unwrap();
        assert_eq!(exclusive.client_id, "pid 100 alice");
        assert_eq!(exclusive.lease_type, "exclusive");
        assert_eq!(exclusive.description, "deploy");
        assert_eq!(response.exclusive_holder.as_deref(), Some("pid 100 alice"));
        assert_eq!(response.monitor_count, 1);
        assert_eq!(response.monitor_leases[0].client_id, "pid 200 bob");
        assert_eq!(response.monitor_leases[0].lease_type, "monitor");
    }

    #[test]
    fn device_info_includes_lease_summary_fields() {
        let manager = DeviceManager::new();
        manager.insert_test_device("COM4");
        manager
            .acquire_exclusive("COM4", "pid 300 carol", "autoresearch")
            .unwrap();

        let state = manager.get_device_status("COM4").unwrap();
        let info = device_info(&state);

        assert_eq!(info.port, "COM4");
        assert!(!info.available_for_exclusive);
        assert_eq!(info.monitor_count, 0);
        assert_eq!(
            info.exclusive_lease.as_ref().map(|l| l.client_id.as_str()),
            Some("pid 300 carol")
        );
    }

    #[test]
    fn exclusive_conflict_maps_to_structured_payload() {
        let manager = DeviceManager::new();
        manager.insert_test_device("COM5");
        manager
            .acquire_exclusive("COM5", "pid 400 dave", "first deploy")
            .unwrap();
        let err = manager
            .acquire_exclusive("COM5", "pid 500 erin", "second deploy")
            .unwrap_err();

        let conflict = lease_conflict(&err).expect("exclusive conflict payload");

        assert_eq!(conflict.port, "COM5");
        assert_eq!(conflict.device_id, "1234:5678");
        assert_eq!(conflict.holder.client_id, "pid 400 dave");
        assert_eq!(conflict.holder.description, "first deploy");
        assert_eq!(conflict.holder.lease_type, "exclusive");
    }
}
