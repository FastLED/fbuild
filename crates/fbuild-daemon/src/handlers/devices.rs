//! Device discovery and management handlers.

use crate::context::DaemonContext;
use crate::models::{DeviceInfo, DeviceListResponse};
use axum::extract::State;
use axum::Json;
use std::sync::Arc;

/// POST /api/devices/list
pub async fn list_devices(_state: State<Arc<DaemonContext>>) -> Json<DeviceListResponse> {
    let devices = match serialport::available_ports() {
        Ok(ports) => ports
            .into_iter()
            .map(|p| {
                let (vid, pid, description) = match &p.port_type {
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
                DeviceInfo {
                    port: p.port_name,
                    device_id: vid.map(|v| format!("{:04x}:{:04x}", v, pid.unwrap_or(0))),
                    vid,
                    pid,
                    description,
                }
            })
            .collect(),
        Err(e) => {
            tracing::warn!("failed to enumerate serial ports: {}", e);
            vec![]
        }
    };

    Json(DeviceListResponse {
        success: true,
        devices,
    })
}
