//! `fbuild device` subcommand: list / status / lease / release / take.

use crate::daemon_client::{
    self, DaemonClient, DeviceLeaseConflictResponse, DeviceLeaseInfoResponse,
};
use crate::output;

use super::args::DeviceAction;

pub async fn run_device(action: DeviceAction) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

    match action {
        DeviceAction::List { refresh } => {
            let resp = client.list_devices(refresh).await?;
            if resp.devices.is_empty() {
                output::result("no devices found");
                return Ok(());
            }
            output::result(format!(
                "{:<20} {:<12} {:<12} {:<24} DESCRIPTION",
                "PORT", "DEVICE ID", "LEASE", "HOLDER"
            ));
            output::result("-".repeat(88));
            for dev in &resp.devices {
                let id = dev.device_id.as_deref().unwrap_or("-");
                let lease = lease_summary(dev.exclusive_lease.as_ref(), dev.monitor_count);
                let holder = dev
                    .exclusive_lease
                    .as_ref()
                    .map(|l| l.client_id.as_str())
                    .unwrap_or("-");
                // Prefer the resolver-derived "vendor product (VID:PID)"
                // display over the raw daemon `description`. When no
                // vendor/product was resolved (non-USB ports, or daemon
                // older than the resolver wiring), fall back to the raw
                // description so behavior is identical to pre-resolver.
                let pretty = device_pretty_name(dev);
                let pretty = with_cdc_suffix(pretty, dev.vid, dev.is_cdc);
                output::result(format!(
                    "{:<20} {:<12} {:<12} {:<24} {}",
                    dev.port,
                    id,
                    lease,
                    holder,
                    device_description(&pretty, dev.previous_port.as_deref())
                ));
            }
            output::result(format!("\n{} device(s) found", resp.devices.len()));
        }
        DeviceAction::Status { port } => {
            let resp = client.device_status(&port).await?;
            if !resp.success {
                output::error(&resp.description);
                return Ok(());
            }
            let connected = if resp.is_connected {
                "connected"
            } else {
                "disconnected"
            };
            output::result(format!("  {}", resp.port));
            output::result(format!("    Device ID: {}", resp.device_id));
            if let (Some(vid), Some(pid)) = (resp.vid, resp.pid) {
                output::result(format!("    USB ID: {vid:04X}:{pid:04X}"));
            }
            if let Some(ref vendor) = resp.vendor_name {
                output::result(format!("    Vendor: {}", vendor));
            }
            if let Some(ref product) = resp.product_name {
                output::result(format!("    Product: {}", product));
            }
            if resp.vid.is_some() {
                output::result(format!("    CDC: {}", cdc_label(resp.is_cdc)));
            }
            output::result(format!("    Description: {}", resp.description));
            if let Some(ref serial) = resp.serial_number {
                output::result(format!("    Serial: {}", serial));
            }
            if let Some(ref previous_port) = resp.previous_port {
                output::result(format!("    Previous port: {}", previous_port));
            }
            output::result(format!("    Status: {}", connected));
            output::result(format!(
                "    Available: {}",
                if resp.available_for_exclusive {
                    "yes"
                } else {
                    "no"
                }
            ));
            if let Some(ref holder) = resp.exclusive_holder {
                output::result(format!("    Exclusive holder: {}", holder));
            }
            if let Some(ref lease) = resp.exclusive_lease {
                print_lease("    Exclusive lease", lease);
            }
            if resp.monitor_count > 0 {
                output::result(format!("    Monitor sessions: {}", resp.monitor_count));
                for lease in &resp.monitor_leases {
                    print_lease("      Monitor lease", lease);
                }
            }
        }
        DeviceAction::Lease {
            port,
            lease_type,
            description,
            track_serial,
        } => {
            let resp = client
                .device_lease(&port, &lease_type, &description, track_serial)
                .await?;
            if resp.success {
                output::result(format!("lease acquired on '{}'", port));
                if let Some(ref id) = resp.lease_id {
                    output::result(format!("  lease_id: {}", id));
                }
            } else {
                output::error(&resp.message);
                if let Some(ref conflict) = resp.conflict {
                    print_conflict(conflict);
                }
            }
        }
        DeviceAction::Release { port, lease_id } => {
            let resp = client.device_release(&port, lease_id.as_deref()).await?;
            if resp.success {
                output::result(&resp.message);
            } else {
                output::error(&resp.message);
            }
        }
        DeviceAction::Take { port, reason } => {
            let resp = client.device_preempt(&port, &reason).await?;
            if resp.success {
                output::result(&resp.message);
            } else {
                output::error(&resp.message);
            }
        }
    }
    Ok(())
}

fn lease_summary(exclusive: Option<&DeviceLeaseInfoResponse>, monitor_count: usize) -> String {
    match (exclusive, monitor_count) {
        (Some(_), 0) => "exclusive".to_string(),
        (Some(_), n) => format!("exclusive+{n}m"),
        (None, 0) => "-".to_string(),
        (None, n) => format!("{n} monitor"),
    }
}

fn print_lease(label: &str, lease: &DeviceLeaseInfoResponse) {
    output::result(format!("{}:", label));
    output::result(format!("      lease_id: {}", lease.lease_id));
    output::result(format!("      type: {}", lease.lease_type));
    output::result(format!("      client_id: {}", lease.client_id));
    output::result(format!(
        "      description: {}",
        empty_dash(&lease.description)
    ));
    output::result(format!("      acquired_at: {:.3}", lease.acquired_at));
    output::result(format!("      track_serial: {}", lease.track_serial));
}

fn print_conflict(conflict: &DeviceLeaseConflictResponse) {
    output::error(format!(
        "  holder: {} ({})",
        conflict.holder.client_id,
        empty_dash(&conflict.holder.description)
    ));
    output::error(format!("  lease_id: {}", conflict.holder.lease_id));
    output::error(format!(
        "  device: {} {}",
        conflict.port, conflict.device_id
    ));
    output::error(format!(
        "  description: {}",
        empty_dash(&conflict.description)
    ));
}

fn empty_dash(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}

fn device_description(description: &str, previous_port: Option<&str>) -> String {
    match previous_port {
        Some(port) => format!("{description} (renum from {port})"),
        None => description.to_string(),
    }
}

fn with_cdc_suffix(description: String, vid: Option<u16>, is_cdc: Option<bool>) -> String {
    if vid.is_some() {
        format!("{description} [cdc={}]", cdc_label(is_cdc))
    } else {
        description
    }
}

fn cdc_label(is_cdc: Option<bool>) -> &'static str {
    match is_cdc {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

/// Compose the canonical `"vendor product (VVVV:PPPP)"` display string
/// for a device row. Falls back to the daemon-provided `description`
/// (and bare hex VID:PID, when available) so this code remains usable
/// against older daemons that don't yet emit `vendor_name`/`product_name`.
fn device_pretty_name(dev: &crate::daemon_client::DeviceInfoResponse) -> String {
    match (
        dev.vid,
        dev.pid,
        dev.vendor_name.as_deref(),
        dev.product_name.as_deref(),
    ) {
        (Some(v), Some(p), Some(vendor), Some(product)) => {
            format!("{vendor} {product} ({v:04X}:{p:04X})")
        }
        (Some(v), Some(p), _, _) => format!("{} ({:04X}:{:04X})", dev.description, v, p),
        _ => dev.description.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdc_suffix_only_applies_to_usb_devices() {
        assert_eq!(
            with_cdc_suffix("Espressif ESP32-S3".to_string(), Some(0x303A), Some(true)),
            "Espressif ESP32-S3 [cdc=yes]"
        );
        assert_eq!(
            with_cdc_suffix("CP210x bridge".to_string(), Some(0x10C4), Some(false)),
            "CP210x bridge [cdc=no]"
        );
        assert_eq!(
            with_cdc_suffix("USB device".to_string(), Some(0x303A), None),
            "USB device [cdc=unknown]"
        );
        assert_eq!(
            with_cdc_suffix("Bluetooth Serial".to_string(), None, None),
            "Bluetooth Serial"
        );
    }
}
