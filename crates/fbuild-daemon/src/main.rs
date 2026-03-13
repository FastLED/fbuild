use clap::Parser;

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
        std::env::set_var("FBUILD_DEV_MODE", "1");
    }

    let port = args.port.unwrap_or_else(fbuild_paths::get_daemon_port);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("fbuild daemon starting on port {}", port);

    // TODO: build axum router, start server
    // let app = axum::Router::new()
    //     .route("/api/daemon/health", get(health))
    //     .route("/api/daemon/info", get(info))
    //     .route("/api/daemon/shutdown", post(shutdown))
    //     .route("/api/operations/build", post(build))
    //     ...
}
