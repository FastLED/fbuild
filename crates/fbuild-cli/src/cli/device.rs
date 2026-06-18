//! `fbuild device` subcommand: list / status / lease / release / take.

use crate::daemon_client::{
    self, DaemonClient, DeviceLeaseConflictResponse, DeviceLeaseInfoResponse,
};

use super::args::DeviceAction;

pub async fn run_device(action: DeviceAction) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

    match action {
        DeviceAction::List { refresh } => {
            let resp = client.list_devices(refresh).await?;
            if resp.devices.is_empty() {
                println!("no devices found");
                return Ok(());
            }
            println!(
                "{:<20} {:<12} {:<12} {:<24} DESCRIPTION",
                "PORT", "DEVICE ID", "LEASE", "HOLDER"
            );
            println!("{}", "-".repeat(88));
            for dev in &resp.devices {
                let id = dev.device_id.as_deref().unwrap_or("-");
                let lease = lease_summary(dev.exclusive_lease.as_ref(), dev.monitor_count);
                let holder = dev
                    .exclusive_lease
                    .as_ref()
                    .map(|l| l.client_id.as_str())
                    .unwrap_or("-");
                println!(
                    "{:<20} {:<12} {:<12} {:<24} {}",
                    dev.port,
                    id,
                    lease,
                    holder,
                    device_description(&dev.description, dev.previous_port.as_deref())
                );
            }
            println!("\n{} device(s) found", resp.devices.len());
        }
        DeviceAction::Status { port } => {
            let resp = client.device_status(&port).await?;
            if !resp.success {
                eprintln!("error: {}", resp.description);
                return Ok(());
            }
            let connected = if resp.is_connected {
                "connected"
            } else {
                "disconnected"
            };
            println!("  {}", resp.port);
            println!("    Device ID: {}", resp.device_id);
            println!("    Description: {}", resp.description);
            if let Some(ref serial) = resp.serial_number {
                println!("    Serial: {}", serial);
            }
            if let Some(ref previous_port) = resp.previous_port {
                println!("    Previous port: {}", previous_port);
            }
            println!("    Status: {}", connected);
            println!(
                "    Available: {}",
                if resp.available_for_exclusive {
                    "yes"
                } else {
                    "no"
                }
            );
            if let Some(ref holder) = resp.exclusive_holder {
                println!("    Exclusive holder: {}", holder);
            }
            if let Some(ref lease) = resp.exclusive_lease {
                print_lease("    Exclusive lease", lease);
            }
            if resp.monitor_count > 0 {
                println!("    Monitor sessions: {}", resp.monitor_count);
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
                println!("lease acquired on '{}'", port);
                if let Some(ref id) = resp.lease_id {
                    println!("  lease_id: {}", id);
                }
            } else {
                eprintln!("error: {}", resp.message);
                if let Some(ref conflict) = resp.conflict {
                    print_conflict(conflict);
                }
            }
        }
        DeviceAction::Release { port, lease_id } => {
            let resp = client.device_release(&port, lease_id.as_deref()).await?;
            if resp.success {
                println!("{}", resp.message);
            } else {
                eprintln!("error: {}", resp.message);
            }
        }
        DeviceAction::Take { port, reason } => {
            let resp = client.device_preempt(&port, &reason).await?;
            if resp.success {
                println!("{}", resp.message);
            } else {
                eprintln!("error: {}", resp.message);
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
    println!("{}:", label);
    println!("      lease_id: {}", lease.lease_id);
    println!("      type: {}", lease.lease_type);
    println!("      client_id: {}", lease.client_id);
    println!("      description: {}", empty_dash(&lease.description));
    println!("      acquired_at: {:.3}", lease.acquired_at);
    println!("      track_serial: {}", lease.track_serial);
}

fn print_conflict(conflict: &DeviceLeaseConflictResponse) {
    eprintln!(
        "  holder: {} ({})",
        conflict.holder.client_id,
        empty_dash(&conflict.holder.description)
    );
    eprintln!("  lease_id: {}", conflict.holder.lease_id);
    eprintln!("  device: {} {}", conflict.port, conflict.device_id);
    eprintln!("  description: {}", empty_dash(&conflict.description));
}

fn empty_dash(value: &str) -> &str {
    if value.is_empty() {
        "-"
    } else {
        value
    }
}

fn device_description(description: &str, previous_port: Option<&str>) -> String {
    match previous_port {
        Some(port) => format!("{description} (renum from {port})"),
        None => description.to_string(),
    }
}
