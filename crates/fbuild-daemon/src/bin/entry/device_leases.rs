//! Restore durable reservations before starting the daemon router.

use crate::context::{BroadcastHub, DaemonContext};
use std::sync::Arc;

pub(super) async fn initialize_context(
    port: u16,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    spawner_cwd: String,
    broadcast_hub: BroadcastHub,
) -> Result<Arc<DaemonContext>, String> {
    let path = fbuild_paths::get_daemon_dir().join("exclusive-device-leases.json");
    let manager = crate::device_manager::DeviceManager::with_lease_store(&path)
        .await
        .map_err(|error| {
            format!(
                "cannot load exclusive device leases from {}: {error}",
                path.display()
            )
        })?;
    let context = Arc::new(DaemonContext::with_device_manager(
        port,
        shutdown_tx,
        spawner_cwd,
        broadcast_hub,
        Arc::new(manager),
    ));
    context.refresh_saved_device_leases().await;

    // FastLED/fbuild#1476: record which image this daemon runs from and who
    // launched it, so a CLI that keeps restarting "stale" daemons can be
    // matched against daemon.log.
    tracing::info!(
        pid = std::process::id(),
        version = env!("CARGO_PKG_VERSION"),
        exe = %context.source_exe,
        source_mtime = context.source_mtime,
        launched_by_broker = context.launched_by_broker,
        "fbuild-daemon identity"
    );
    DaemonContext::install_dependency_status_subscriber(&context);
    crate::broker::backend::spawn_backend_endpoint_if_requested(context.clone());
    Ok(context)
}
