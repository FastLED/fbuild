use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "fbuild",
    version,
    about = "PlatformIO-compatible embedded build tool"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Project directory (positional, for `fbuild <dir>`)
    project_dir: Option<String>,

    /// Target environment
    #[arg(short = 'e', long)]
    environment: Option<String>,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,

    /// Use PlatformIO compatibility mode
    #[arg(long)]
    platformio: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Build firmware
    Build {
        project_dir: String,
        #[arg(short = 'e', long)]
        environment: Option<String>,
        #[arg(long)]
        clean: bool,
        #[arg(short, long)]
        verbose: bool,
        #[arg(long)]
        jobs: Option<usize>,
        #[arg(long)]
        quick: bool,
        #[arg(long)]
        release: bool,
        #[arg(long)]
        platformio: bool,
    },
    /// Deploy firmware to device
    Deploy {
        project_dir: String,
        #[arg(short = 'e', long)]
        environment: Option<String>,
        #[arg(long)]
        port: Option<String>,
        #[arg(long)]
        monitor: bool,
        #[arg(short, long)]
        verbose: bool,
        #[arg(long)]
        platformio: bool,
    },
    /// Monitor serial output
    Monitor {
        project_dir: String,
        #[arg(short = 'e', long)]
        environment: Option<String>,
        #[arg(long)]
        port: Option<String>,
        #[arg(long)]
        baud_rate: Option<u32>,
        #[arg(long)]
        platformio: bool,
    },
    /// Purge cached packages
    Purge {
        target: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        project_dir: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    match cli.command {
        Some(Commands::Build { .. }) => {
            // TODO: send build request to daemon via HTTP
            eprintln!("build not yet implemented");
        }
        Some(Commands::Deploy { .. }) => {
            // TODO: send deploy request to daemon via HTTP
            eprintln!("deploy not yet implemented");
        }
        Some(Commands::Monitor { .. }) => {
            // TODO: send monitor request to daemon via HTTP
            eprintln!("monitor not yet implemented");
        }
        Some(Commands::Purge { .. }) => {
            // TODO: purge cached packages
            eprintln!("purge not yet implemented");
        }
        None => {
            // Default action: build + deploy (like Python fbuild)
            eprintln!("default action not yet implemented");
        }
    }
}
