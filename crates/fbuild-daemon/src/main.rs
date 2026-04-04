#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use fbuild_daemon::context::{
    DaemonContext, IDLE_TIMEOUT, SELF_EVICTION_TIMEOUT, STALE_LOCK_CHECK_INTERVAL,
};
use fbuild_daemon::handlers::{devices, health, locks, operations, websockets};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "fbuild-daemon", about = "fbuild daemon server")]
struct Args {
    /// Port to listen on (default: 8765 prod, 8865 dev)
    #[arg(short, long)]
    port: Option<u16>,

    /// Run in development mode
    #[arg(long)]
    dev: bool,

    /// Working directory of the client that spawned this daemon
    #[arg(long, default_value = "unknown")]
    spawner_cwd: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    if args.dev {
        unsafe { std::env::set_var("FBUILD_DEV_MODE", "1") };
    }

    let port = args.port.unwrap_or_else(fbuild_paths::get_daemon_port);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("fbuild daemon starting on port {}", port);

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let context = Arc::new(DaemonContext::new(port, shutdown_tx, args.spawner_cwd));

    let app = Router::new()
        .route("/", get(health::root))
        .route("/health", get(health::health_check))
        .route("/api/daemon/info", get(health::daemon_info))
        .route("/api/daemon/shutdown", post(health::shutdown))
        .route("/api/build", post(operations::build))
        .route("/api/deploy", post(operations::deploy))
        .route("/api/monitor", post(operations::monitor))
        .route("/api/devices/list", post(devices::list_devices))
        .route("/api/devices/:port/status", get(devices::device_status))
        .route("/api/devices/:port/lease", post(devices::device_lease))
        .route("/api/devices/:port/release", post(devices::device_release))
        .route("/api/devices/:port/preempt", post(devices::device_preempt))
        .route("/api/locks/status", get(locks::lock_status))
        .route("/api/locks/clear", post(locks::clear_locks))
        .route("/api/install-deps", post(operations::install_deps))
        .route("/api/reset", post(operations::reset))
        .route("/ws/serial-monitor", get(websockets::ws_serial_monitor))
        .route("/ws/status", get(websockets::ws_status))
        .route("/ws/logs", get(websockets::ws_logs))
        .route(
            "/ws/monitor/:session_id",
            get(websockets::ws_monitor_session),
        )
        .with_state(context.clone())
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin([
                    "http://localhost".parse().unwrap(),
                    "http://127.0.0.1".parse().unwrap(),
                ])
                .allow_methods(tower_http::cors::Any)
                .allow_headers(tower_http::cors::Any),
        );

    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("failed to bind to {}: {}", addr, e);
            std::process::exit(1);
        });

    tracing::info!("listening on {}", addr);

    // Write PID file and port file
    let pid_file = fbuild_paths::get_daemon_pid_file();
    let port_file = fbuild_paths::get_daemon_port_file();
    if let Some(parent) = pid_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&pid_file, std::process::id().to_string()) {
        tracing::warn!("failed to write PID file: {}", e);
    }
    if let Err(e) = std::fs::write(&port_file, port.to_string()) {
        tracing::warn!("failed to write port file: {}", e);
    }

    // Spawn background maintenance task (self-eviction, idle timeout, stale lock cleanup)
    {
        let ctx = context.clone();
        tokio::spawn(async move {
            let mut daemon_empty_since: Option<std::time::Instant> = None;
            let mut last_lock_cleanup = std::time::Instant::now();

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                // --- Self-eviction: 0 ops + 0 serial sessions for SELF_EVICTION_TIMEOUT ---
                if ctx.is_empty() {
                    if daemon_empty_since.is_none() {
                        daemon_empty_since = Some(std::time::Instant::now());
                        tracing::debug!(
                            "Daemon is now empty (0 ops, 0 serial sessions), starting eviction timer"
                        );
                    } else if let Some(since) = daemon_empty_since {
                        if since.elapsed() >= SELF_EVICTION_TIMEOUT {
                            tracing::info!(
                                "Self-eviction triggered: daemon empty for {:.1}s, shutting down",
                                since.elapsed().as_secs_f64()
                            );
                            let _ = ctx.shutdown_tx.send(true);
                            return;
                        }
                    }
                } else if daemon_empty_since.is_some() {
                    tracing::debug!("Daemon is no longer empty, resetting eviction timer");
                    daemon_empty_since = None;
                }

                // --- Idle timeout (12h fallback) ---
                if ctx.idle_duration() >= IDLE_TIMEOUT {
                    tracing::info!(
                        "Idle timeout reached ({:.0}s), shutting down",
                        ctx.idle_duration().as_secs_f64()
                    );
                    let _ = ctx.shutdown_tx.send(true);
                    return;
                }

                // --- Periodic stale lock cleanup ---
                if last_lock_cleanup.elapsed() >= STALE_LOCK_CHECK_INTERVAL {
                    last_lock_cleanup = std::time::Instant::now();
                    let mut cleared = 0usize;
                    let keys: Vec<_> = ctx.project_locks.iter().map(|e| e.key().clone()).collect();
                    for key in keys {
                        if let Some(entry) = ctx.project_locks.get(&key) {
                            if entry.value().try_lock().is_ok() {
                                drop(entry);
                                ctx.project_locks.remove(&key);
                                cleared += 1;
                            }
                        }
                    }
                    if cleared > 0 {
                        tracing::info!(
                            "Periodic cleanup: cleared {} stale project lock(s)",
                            cleared
                        );
                    }
                }
            }
        });
    }

    // Clone refs so we can check operation state on Ctrl+C
    let shutdown_tx_signal = context.shutdown_tx.clone();
    let op_in_progress = context.operation_in_progress.clone();

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Wait for either the HTTP shutdown endpoint or Ctrl+C / SIGTERM
            tokio::select! {
                _ = async { let _ = shutdown_rx.changed().await; } => {
                    tracing::info!("shutdown requested via HTTP");
                }
                _ = async {
                    // On Ctrl+C, refuse shutdown if an operation is in progress
                    // (matches Python daemon behavior)
                    loop {
                        tokio::signal::ctrl_c().await.ok();
                        if op_in_progress.load(std::sync::atomic::Ordering::Relaxed) {
                            tracing::warn!("SIGINT received during operation — ignoring. Use kill -9 to force.");
                            eprintln!("⚠ Cannot shutdown while operation is active. Use kill -9 to force.");
                        } else {
                            tracing::info!("shutdown requested via Ctrl+C");
                            let _ = shutdown_tx_signal.send(true);
                            break;
                        }
                    }
                } => {}
            }
            tracing::info!("graceful shutdown initiated");
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!("server error: {}", e);
        });

    // Clean up PID and port files
    let _ = std::fs::remove_file(&pid_file);
    let _ = std::fs::remove_file(&port_file);

    tracing::info!("daemon exiting");
    std::process::exit(0);
}
