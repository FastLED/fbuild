//! Device discovery and management handlers.

use crate::context::DaemonContext;
use crate::device_manager::LeaseType;
use crate::models::{
    DeviceInfo, DeviceLeaseRequest, DeviceLeaseResponse, DeviceListResponse, DevicePreemptRequest,
    DevicePreemptResponse, DeviceReleaseRequest, DeviceReleaseResponse, DeviceStatusResponse,
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
    let devices = all
        .values()
        .map(|s| DeviceInfo {
            port: s.port.clone(),
            device_id: Some(s.device_id.clone()),
            vid: s.vid,
            pid: s.pid,
            description: s.description.clone(),
        })
        .collect();

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
        Some(ds) => {
            let available = ds.is_available_for_exclusive();
            let holder = ds.exclusive_lease.as_ref().map(|l| l.client_id.clone());
            let monitors = ds.monitor_count();
            Json(DeviceStatusResponse {
                success: true,
                port: ds.port,
                device_id: ds.device_id,
                description: ds.description,
                is_connected: ds.is_connected,
                available_for_exclusive: available,
                exclusive_holder: holder,
                monitor_count: monitors,
            })
        }
        None => Json(DeviceStatusResponse {
            success: false,
            port: port.clone(),
            device_id: String::new(),
            description: format!("device '{}' not found", port),
            is_connected: false,
            available_for_exclusive: false,
            exclusive_holder: None,
            monitor_count: 0,
        }),
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
        other => Err(format!(
            "invalid lease_type '{}', must be 'exclusive' or 'monitor'",
            other
        )),
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
        }),
        Err(msg) => Json(DeviceLeaseResponse {
            success: false,
            lease_id: None,
            lease_type: None,
            message: msg,
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
            message: msg,
        }),
    }
}
