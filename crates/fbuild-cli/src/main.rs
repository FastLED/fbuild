mod daemon_client;

use clap::{Parser, Subcommand};
use daemon_client::{BuildRequest, DaemonClient, DeployRequest, MonitorRequest};

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

    /// Monitor timeout in seconds
    #[arg(long)]
    timeout: Option<f64>,

    /// Halt monitor on error pattern
    #[arg(long)]
    halt_on_error: Option<String>,

    /// Halt monitor on success pattern
    #[arg(long)]
    halt_on_success: Option<String>,

    /// Expected output pattern for monitor
    #[arg(long)]
    expect: Option<String>,
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
        #[arg(long)]
        timeout: Option<f64>,
        #[arg(long)]
        halt_on_error: Option<String>,
        #[arg(long)]
        halt_on_success: Option<String>,
        #[arg(long)]
        expect: Option<String>,
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
        #[arg(long)]
        timeout: Option<f64>,
        #[arg(long)]
        halt_on_error: Option<String>,
        #[arg(long)]
        halt_on_success: Option<String>,
        #[arg(long)]
        expect: Option<String>,
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

    let result = match cli.command {
        Some(Commands::Build {
            project_dir,
            environment,
            clean,
            verbose,
            jobs,
            quick,
            release: _,
            platformio: _,
        }) => run_build(project_dir, environment, clean, verbose, jobs, quick).await,
        Some(Commands::Deploy {
            project_dir,
            environment,
            port,
            monitor,
            verbose,
            platformio: _,
            timeout,
            halt_on_error,
            halt_on_success,
            expect,
        }) => {
            run_deploy(
                project_dir,
                environment,
                port,
                monitor,
                verbose,
                timeout,
                halt_on_error,
                halt_on_success,
                expect,
            )
            .await
        }
        Some(Commands::Monitor {
            project_dir,
            environment,
            port,
            baud_rate,
            platformio: _,
            timeout,
            halt_on_error,
            halt_on_success,
            expect,
        }) => {
            run_monitor(
                project_dir,
                environment,
                port,
                baud_rate,
                timeout,
                halt_on_error,
                halt_on_success,
                expect,
            )
            .await
        }
        Some(Commands::Purge {
            target,
            dry_run,
            project_dir,
        }) => run_purge(target, dry_run, project_dir),
        None => {
            // Default action: deploy with monitor (like Python fbuild)
            let project_dir = cli.project_dir.unwrap_or_else(|| ".".to_string());
            run_default(
                project_dir,
                cli.environment,
                cli.verbose,
                cli.timeout,
                cli.halt_on_error,
                cli.halt_on_success,
                cli.expect,
            )
            .await
        }
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

async fn run_build(
    project_dir: String,
    environment: Option<String>,
    clean: bool,
    verbose: bool,
    jobs: Option<usize>,
    quick: bool,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

    let profile = if quick {
        Some("quick".to_string())
    } else {
        None
    };
    let req = BuildRequest {
        project_dir,
        environment,
        clean_build: clean,
        verbose,
        jobs,
        profile,
        request_id: None,
    };

    let resp = client.build(&req).await?;
    println!("{}", resp.message);
    if !resp.success {
        std::process::exit(resp.exit_code);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_deploy(
    project_dir: String,
    environment: Option<String>,
    port: Option<String>,
    monitor_after: bool,
    verbose: bool,
    timeout: Option<f64>,
    halt_on_error: Option<String>,
    halt_on_success: Option<String>,
    expect: Option<String>,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

    let req = DeployRequest {
        project_dir,
        environment,
        port,
        monitor_after,
        skip_build: false,
        clean_build: false,
        verbose,
        monitor_timeout: timeout,
        monitor_halt_on_error: halt_on_error,
        monitor_halt_on_success: halt_on_success,
        monitor_expect: expect,
        request_id: None,
    };

    let resp = client.deploy(&req).await?;
    println!("{}", resp.message);
    if !resp.success {
        std::process::exit(resp.exit_code);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_monitor(
    project_dir: String,
    environment: Option<String>,
    port: Option<String>,
    baud_rate: Option<u32>,
    timeout: Option<f64>,
    halt_on_error: Option<String>,
    halt_on_success: Option<String>,
    expect: Option<String>,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

    let req = MonitorRequest {
        project_dir,
        environment,
        port,
        baud_rate,
        halt_on_error,
        halt_on_success,
        expect,
        timeout,
        request_id: None,
    };

    let resp = client.monitor(&req).await?;
    println!("{}", resp.message);
    if !resp.success {
        std::process::exit(resp.exit_code);
    }
    Ok(())
}

fn run_purge(
    target: Option<String>,
    dry_run: bool,
    project_dir: Option<String>,
) -> fbuild_core::Result<()> {
    let cache_root = fbuild_paths::get_cache_root();

    if let Some(ref t) = target {
        let path = cache_root.join(t);
        if !path.exists() {
            eprintln!("target not found: {}", path.display());
            return Ok(());
        }
        if dry_run {
            println!("would remove: {}", path.display());
        } else {
            std::fs::remove_dir_all(&path).map_err(|e| {
                fbuild_core::FbuildError::Other(format!(
                    "failed to remove {}: {}",
                    path.display(),
                    e
                ))
            })?;
            println!("removed: {}", path.display());
        }
    } else {
        // Purge entire cache
        if dry_run {
            println!("would remove: {}", cache_root.display());
            if let Some(ref pd) = project_dir {
                let build_root = fbuild_paths::get_project_build_root(std::path::Path::new(pd));
                println!("would remove: {}", build_root.display());
            }
        } else {
            if cache_root.exists() {
                std::fs::remove_dir_all(&cache_root).map_err(|e| {
                    fbuild_core::FbuildError::Other(format!(
                        "failed to remove {}: {}",
                        cache_root.display(),
                        e
                    ))
                })?;
                println!("removed: {}", cache_root.display());
            }
            if let Some(ref pd) = project_dir {
                let build_root = fbuild_paths::get_project_build_root(std::path::Path::new(pd));
                if build_root.exists() {
                    std::fs::remove_dir_all(&build_root).map_err(|e| {
                        fbuild_core::FbuildError::Other(format!(
                            "failed to remove {}: {}",
                            build_root.display(),
                            e
                        ))
                    })?;
                    println!("removed: {}", build_root.display());
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_default(
    project_dir: String,
    environment: Option<String>,
    verbose: bool,
    timeout: Option<f64>,
    halt_on_error: Option<String>,
    halt_on_success: Option<String>,
    expect: Option<String>,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

    // Deploy with monitor (Python fbuild default behavior)
    let deploy_req = DeployRequest {
        project_dir,
        environment,
        port: None,
        monitor_after: true,
        skip_build: false,
        clean_build: false,
        verbose,
        monitor_timeout: timeout,
        monitor_halt_on_error: halt_on_error,
        monitor_halt_on_success: halt_on_success,
        monitor_expect: expect,
        request_id: None,
    };

    let deploy_resp = client.deploy(&deploy_req).await?;
    println!("{}", deploy_resp.message);
    if !deploy_resp.success {
        std::process::exit(deploy_resp.exit_code);
    }

    Ok(())
}
