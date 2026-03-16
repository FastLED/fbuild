use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use fbuild_daemon::context::DaemonContext;
use fbuild_daemon::handlers::{devices, health, operations, websockets};
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
    let context = Arc::new(DaemonContext::new(port, shutdown_tx));

    let app = Router::new()
        .route("/health", get(health::health_check))
        .route("/api/daemon/info", get(health::daemon_info))
        .route("/api/daemon/shutdown", post(health::shutdown))
        .route("/api/build", post(operations::build))
        .route("/api/deploy", post(operations::deploy))
        .route("/api/monitor", post(operations::monitor))
        .route("/api/devices/list", post(devices::list_devices))
        .route("/ws/serial-monitor", get(websockets::ws_serial_monitor))
        .with_state(context.clone());

    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("failed to bind to {}: {}", addr, e);
            std::process::exit(1);
        });

    tracing::info!("listening on {}", addr);

    // Clone shutdown_tx so we can trigger shutdown from Ctrl+C too
    let shutdown_tx_signal = context.shutdown_tx.clone();

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Wait for either the HTTP shutdown endpoint or Ctrl+C / SIGTERM
            tokio::select! {
                _ = async { let _ = shutdown_rx.changed().await; } => {
                    tracing::info!("shutdown requested via HTTP");
                }
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("shutdown requested via Ctrl+C");
                    let _ = shutdown_tx_signal.send(true);
                }
            }
            tracing::info!("graceful shutdown initiated");
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!("server error: {}", e);
        });

    tracing::info!("daemon exiting");
    std::process::exit(0);
}
