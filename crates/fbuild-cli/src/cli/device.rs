//! `fbuild device` subcommand: list / status / lease / release / take.

use crate::daemon_client::{self, DaemonClient};

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
            println!("{:<20} {:<12} {:<20}", "PORT", "DEVICE ID", "DESCRIPTION");
            println!("{}", "-".repeat(52));
            for dev in &resp.devices {
                let id = dev.device_id.as_deref().unwrap_or("-");
                println!("{:<20} {:<12} {:<20}", dev.port, id, dev.description);
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
            if resp.monitor_count > 0 {
                println!("    Monitor sessions: {}", resp.monitor_count);
            }
        }
        DeviceAction::Lease {
            port,
            lease_type,
            description,
        } => {
            let resp = client
                .device_lease(&port, &lease_type, &description)
                .await?;
            if resp.success {
                println!("lease acquired on '{}'", port);
                if let Some(ref id) = resp.lease_id {
                    println!("  lease_id: {}", id);
                }
            } else {
                eprintln!("error: {}", resp.message);
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
