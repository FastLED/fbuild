mod daemon_client;
mod mcp;

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

    /// Serial port (e.g., COM5, /dev/ttyUSB0)
    #[arg(short = 'p', long)]
    port: Option<String>,

    /// Clean build before deploy
    #[arg(short = 'c', long)]
    clean: bool,

    /// Monitor after deploy; optionally pass flags as a string
    #[arg(long, num_args = 0..=1, default_missing_value = "")]
    monitor: Option<String>,

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
        #[arg(short = 'c', long)]
        clean: bool,
        #[arg(short, long)]
        verbose: bool,
        #[arg(short = 'j', long, value_parser = parse_jobs)]
        jobs: Option<usize>,
        #[arg(long, group = "build_profile")]
        quick: bool,
        #[arg(long, group = "build_profile")]
        release: bool,
        #[arg(long)]
        platformio: bool,
        /// Verify daemon starts and environment resolves, but skip the actual build
        #[arg(long)]
        dry_run: bool,
        /// Build target: 'compiledb' generates compile_commands.json without compiling
        #[arg(short = 't', long, value_parser = ["compiledb"])]
        target: Option<String>,
    },
    /// Deploy firmware to device
    Deploy {
        project_dir: String,
        #[arg(short = 'e', long)]
        environment: Option<String>,
        #[arg(short = 'p', long)]
        port: Option<String>,
        #[arg(short = 'c', long)]
        clean: bool,
        /// Monitor after deploy; optionally pass flags as a string
        /// e.g., --monitor="--timeout 60 --halt-on-success \"TEST PASSED\""
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        monitor: Option<String>,
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
        /// Disable timestamp prefix on monitor output lines
        #[arg(long)]
        no_timestamp: bool,
        /// Skip the build step and deploy existing firmware (upload-only mode)
        #[arg(long)]
        skip_build: bool,
        /// Deploy to QEMU emulator instead of physical device (requires Docker)
        #[arg(long)]
        qemu: bool,
        /// Timeout in seconds for QEMU execution (default: 30)
        #[arg(long, default_value = "30")]
        qemu_timeout: u32,
    },
    /// Monitor serial output
    Monitor {
        project_dir: String,
        #[arg(short = 'e', long)]
        environment: Option<String>,
        #[arg(short = 'p', long)]
        port: Option<String>,
        #[arg(short = 'b', long = "baud", alias = "baud-rate")]
        baud_rate: Option<u32>,
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
        /// Disable timestamp prefix on each output line
        #[arg(long)]
        no_timestamp: bool,
    },
    /// Reset device without re-flashing
    Reset {
        /// Project directory
        #[arg(default_value = ".")]
        project_dir: String,
        /// Target environment
        #[arg(short = 'e', long)]
        environment: Option<String>,
        /// Serial port (e.g., COM5, /dev/ttyUSB0)
        #[arg(short = 'p', long)]
        port: Option<String>,
        /// Verbose output
        #[arg(short, long)]
        verbose: bool,
    },
    /// Purge cached packages
    Purge {
        target: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        project_dir: Option<String>,
    },
    /// Manage the fbuild daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Show daemon logs or other information
    Show {
        /// What to show (currently only 'daemon' for daemon logs)
        target: String,
        /// Don't follow the log file (just print last lines and exit)
        #[arg(long)]
        no_follow: bool,
        /// Number of lines to show initially (default: 50)
        #[arg(long, default_value = "50")]
        lines: usize,
    },
    /// Manage connected devices
    Device {
        #[command(subcommand)]
        action: DeviceAction,
    },
    /// Start MCP (Model Context Protocol) server for AI assistant integration
    Mcp,
}

#[derive(Subcommand)]
enum DeviceAction {
    /// List all connected devices
    List {
        /// Refresh device discovery before listing
        #[arg(long)]
        refresh: bool,
    },
    /// Show detailed status of a device
    Status {
        /// Serial port (e.g. COM3, /dev/ttyUSB0)
        port: String,
    },
    /// Acquire a lease on a device
    Lease {
        /// Serial port (e.g. COM3, /dev/ttyUSB0)
        port: String,
        /// Lease type: "exclusive" (default) or "monitor"
        #[arg(short = 't', long, default_value = "exclusive")]
        lease_type: String,
        /// Description for the lease
        #[arg(short, long, default_value = "")]
        description: String,
    },
    /// Release a lease on a device
    Release {
        /// Serial port (e.g. COM3, /dev/ttyUSB0)
        port: String,
        /// Specific lease ID to release (releases all if omitted)
        #[arg(long)]
        lease_id: Option<String>,
    },
    /// Forcibly take a device from the current holder
    Take {
        /// Serial port (e.g. COM3, /dev/ttyUSB0)
        port: String,
        /// Mandatory reason for preemption
        #[arg(short, long)]
        reason: String,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Stop the daemon gracefully
    Stop,
    /// Show daemon status
    Status,
    /// Restart the daemon (stop then start)
    Restart,
    /// List running daemon instances
    List,
    /// Kill a daemon process (bypasses graceful shutdown)
    Kill {
        /// PID of the daemon to kill (auto-detected if omitted)
        #[arg(long)]
        pid: Option<u32>,
        /// Force kill (SIGKILL/TerminateProcess) instead of graceful termination
        #[arg(short, long)]
        force: bool,
    },
    /// Kill all fbuild-daemon processes
    KillAll {
        /// Force kill (SIGKILL/TerminateProcess) instead of graceful termination
        #[arg(short, long)]
        force: bool,
    },
    /// Show lock status (project locks, serial sessions)
    Locks,
    /// Clear stale locks
    ClearLocks,
    /// Tail daemon logs (alias for `fbuild show daemon`)
    Monitor {
        /// Don't follow the log file (just print last lines and exit)
        #[arg(long)]
        no_follow: bool,
        /// Number of lines to show initially
        #[arg(long, default_value = "50")]
        lines: usize,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Handle Ctrl+C with exit code 130 (standard POSIX SIGINT behavior, matches Python)
    ctrlc::set_handler(move || {
        eprintln!("\nInterrupted");
        std::process::exit(130);
    })
    .ok();

    // Notify when running in dev mode (matches Python behavior)
    if std::env::var("FBUILD_DEV_MODE").is_ok_and(|v| v == "1") {
        eprintln!("FBUILD_DEV_MODE=1 (dev mode: port 8865, ~/.fbuild/dev/)");
    }

    // Show compact daemon status before every command (matches Python behavior)
    daemon_client::display_daemon_stats_compact().await;

    let result = match cli.command {
        Some(Commands::Build {
            project_dir,
            environment,
            clean,
            verbose,
            jobs,
            quick,
            release,
            platformio,
            dry_run,
            target,
        }) => {
            if platformio {
                pio_build(&project_dir, environment.as_deref(), clean, verbose)
            } else {
                run_build(
                    project_dir,
                    environment,
                    clean,
                    verbose,
                    jobs,
                    quick,
                    release,
                    dry_run,
                    target,
                )
                .await
            }
        }
        Some(Commands::Deploy {
            project_dir,
            environment,
            port,
            clean,
            monitor,
            verbose,
            platformio,
            timeout,
            halt_on_error,
            halt_on_success,
            expect,
            no_timestamp,
            skip_build,
            qemu,
            qemu_timeout,
        }) => {
            if platformio {
                pio_deploy(
                    &project_dir,
                    environment.as_deref(),
                    port.as_deref(),
                    clean,
                    verbose,
                )
            } else {
                let monitor_after = monitor.is_some();
                let parsed = monitor
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(parse_monitor_flags)
                    .unwrap_or_default();
                run_deploy(
                    project_dir,
                    environment,
                    port,
                    clean,
                    monitor_after,
                    verbose,
                    timeout.or(parsed.timeout),
                    halt_on_error.or(parsed.halt_on_error),
                    halt_on_success.or(parsed.halt_on_success),
                    expect.or(parsed.expect),
                    no_timestamp,
                    skip_build,
                    qemu,
                    qemu_timeout,
                )
                .await
            }
        }
        Some(Commands::Monitor {
            project_dir,
            environment,
            port,
            baud_rate,
            verbose: _,
            platformio,
            timeout,
            halt_on_error,
            halt_on_success,
            expect,
            no_timestamp,
        }) => {
            if platformio {
                pio_monitor(
                    &project_dir,
                    environment.as_deref(),
                    port.as_deref(),
                    baud_rate,
                )
            } else {
                run_monitor(
                    project_dir,
                    environment,
                    port,
                    baud_rate,
                    timeout,
                    halt_on_error,
                    halt_on_success,
                    expect,
                    no_timestamp,
                )
                .await
            }
        }
        Some(Commands::Reset {
            project_dir,
            environment,
            port,
            verbose,
        }) => run_reset(project_dir, environment, port, verbose),
        Some(Commands::Purge {
            target,
            dry_run,
            project_dir,
        }) => run_purge(target, dry_run, project_dir),
        Some(Commands::Daemon { action }) => run_daemon(action).await,
        Some(Commands::Show {
            target,
            no_follow,
            lines,
        }) => run_show(&target, !no_follow, lines),
        Some(Commands::Device { action }) => run_device(action).await,
        Some(Commands::Mcp) => {
            let code = mcp::run_mcp_server().await;
            if code == 0 {
                Ok(())
            } else {
                Err(fbuild_core::FbuildError::BuildFailed(
                    "MCP server exited with error".to_string(),
                ))
            }
        }
        None => {
            // Default action: deploy with monitor (like Python fbuild)
            let project_dir = cli.project_dir.unwrap_or_else(|| ".".to_string());
            if cli.platformio {
                pio_deploy(
                    &project_dir,
                    cli.environment.as_deref(),
                    cli.port.as_deref(),
                    cli.clean,
                    cli.verbose,
                )
            } else {
                let monitor_after = cli.monitor.as_ref().map_or(true, |_| true);
                let parsed = cli
                    .monitor
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(parse_monitor_flags)
                    .unwrap_or_default();
                run_deploy(
                    project_dir,
                    cli.environment,
                    cli.port,
                    cli.clean,
                    monitor_after,
                    cli.verbose,
                    cli.timeout.or(parsed.timeout),
                    cli.halt_on_error.or(parsed.halt_on_error),
                    cli.halt_on_success.or(parsed.halt_on_success),
                    cli.expect.or(parsed.expect),
                    false,
                    false,
                    false,
                    30,
                )
                .await
            }
        }
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

/// Parsed monitor flags extracted from a `--monitor="..."` string.
#[derive(Default)]
struct ParsedMonitorFlags {
    timeout: Option<f64>,
    halt_on_error: Option<String>,
    halt_on_success: Option<String>,
    expect: Option<String>,
}

/// Validate that jobs count is >= 1 (matches Python behavior).
fn parse_jobs(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|e| format!("{e}"))?;
    if n == 0 {
        return Err("jobs must be >= 1".to_string());
    }
    Ok(n)
}

/// Parse monitor flags from a string like `--timeout 60 --halt-on-success "TEST PASSED"`.
fn parse_monitor_flags(s: &str) -> ParsedMonitorFlags {
    let mut result = ParsedMonitorFlags::default();
    let tokens = shell_tokenize(s);
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "--timeout" | "-t" => {
                if let Some(val) = tokens.get(i + 1) {
                    result.timeout = val.parse().ok();
                    i += 1;
                }
            }
            "--halt-on-error" => {
                if let Some(val) = tokens.get(i + 1) {
                    result.halt_on_error = Some(val.clone());
                    i += 1;
                }
            }
            "--halt-on-success" => {
                if let Some(val) = tokens.get(i + 1) {
                    result.halt_on_success = Some(val.clone());
                    i += 1;
                }
            }
            "--expect" => {
                if let Some(val) = tokens.get(i + 1) {
                    result.expect = Some(val.clone());
                    i += 1;
                }
            }
            other => {
                // Handle --key=value form
                if let Some(rest) = other.strip_prefix("--timeout=") {
                    result.timeout = rest.parse().ok();
                } else if let Some(rest) = other.strip_prefix("--halt-on-error=") {
                    result.halt_on_error = Some(rest.to_string());
                } else if let Some(rest) = other.strip_prefix("--halt-on-success=") {
                    result.halt_on_success = Some(rest.to_string());
                } else if let Some(rest) = other.strip_prefix("--expect=") {
                    result.expect = Some(rest.to_string());
                }
            }
        }
        i += 1;
    }
    result
}

/// Simple shell-style tokenizer that handles quoted strings.
fn shell_tokenize(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape_next = false;

    for ch in s.chars() {
        if escape_next {
            current.push(ch);
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if !in_single_quote => {
                escape_next = true;
            }
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

// ---------------------------------------------------------------------------
// PlatformIO passthrough: delegates to `pio` CLI instead of fbuild daemon
// ---------------------------------------------------------------------------

/// Find the `pio` binary. Checks PATH first, then the fbuild cache.
fn find_pio() -> fbuild_core::Result<std::path::PathBuf> {
    // Check PATH
    if let Ok(output) = std::process::Command::new(if cfg!(windows) { "where" } else { "which" })
        .arg("pio")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !path.is_empty() {
                return Ok(std::path::PathBuf::from(path));
            }
        }
    }

    // Check fbuild cache (PlatformIO installed via iso_env)
    let cache = fbuild_paths::get_cache_root().join("platform");
    let candidates = if cfg!(windows) {
        vec![
            cache.join("Scripts").join("pio.exe"),
            cache.join("Scripts").join("pio"),
        ]
    } else {
        vec![cache.join("bin").join("pio")]
    };
    for c in candidates {
        if c.exists() {
            return Ok(c);
        }
    }

    Err(fbuild_core::FbuildError::Other(
        "PlatformIO not found. Install it with: pip install platformio".to_string(),
    ))
}

/// Run a PlatformIO command with real-time output streaming.
fn run_pio_command(args: &[&str]) -> fbuild_core::Result<()> {
    let pio = find_pio()?;
    let status = std::process::Command::new(&pio)
        .args(args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to run pio: {}", e)))?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn pio_build(
    project_dir: &str,
    environment: Option<&str>,
    clean: bool,
    verbose: bool,
) -> fbuild_core::Result<()> {
    if clean {
        let mut args = vec!["run", "--target", "clean", "-d", project_dir];
        if let Some(env) = environment {
            args.extend(["-e", env]);
        }
        let _ = run_pio_command(&args);
    }
    let mut args = vec!["run", "-d", project_dir];
    if let Some(env) = environment {
        args.extend(["-e", env]);
    }
    if verbose {
        args.push("-v");
    }
    run_pio_command(&args)
}

fn pio_deploy(
    project_dir: &str,
    environment: Option<&str>,
    port: Option<&str>,
    clean: bool,
    verbose: bool,
) -> fbuild_core::Result<()> {
    if clean {
        let mut args = vec!["run", "--target", "clean", "-d", project_dir];
        if let Some(env) = environment {
            args.extend(["-e", env]);
        }
        let _ = run_pio_command(&args);
    }
    let mut args = vec!["run", "--target", "upload", "-d", project_dir];
    if let Some(env) = environment {
        args.extend(["-e", env]);
    }
    if let Some(p) = port {
        args.extend(["--upload-port", p]);
    }
    if verbose {
        args.push("-v");
    }
    run_pio_command(&args)
}

fn pio_monitor(
    project_dir: &str,
    environment: Option<&str>,
    port: Option<&str>,
    baud_rate: Option<u32>,
) -> fbuild_core::Result<()> {
    let baud_str;
    let mut args = vec!["device", "monitor", "-d", project_dir];
    if let Some(env) = environment {
        args.extend(["-e", env]);
    }
    if let Some(p) = port {
        args.extend(["--port", p]);
    }
    if let Some(b) = baud_rate {
        baud_str = b.to_string();
        args.extend(["--baud", &baud_str]);
    }
    run_pio_command(&args)
}

#[allow(clippy::too_many_arguments)]
async fn run_build(
    project_dir: String,
    environment: Option<String>,
    clean: bool,
    verbose: bool,
    jobs: Option<usize>,
    quick: bool,
    release: bool,
    dry_run: bool,
    target: Option<String>,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;

    // Dry-run: verify daemon starts and environment resolves, then exit
    if dry_run {
        let project_path = std::path::Path::new(&project_dir);
        let ini_path = project_path.join("platformio.ini");
        let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
        let env_name = environment
            .as_deref()
            .or_else(|| config.get_default_environment())
            .unwrap_or("default");
        println!("Environment: {}", env_name);
        println!("Daemon is running. Dry-run complete.");
        return Ok(());
    }

    let client = DaemonClient::new();

    let profile = if release {
        Some("release".to_string())
    } else if quick {
        Some("quick".to_string())
    } else {
        None
    };
    let generate_compiledb = target.as_deref() == Some("compiledb");
    if generate_compiledb {
        let env_label = environment.as_deref().unwrap_or("default");
        println!(
            "Generating compile_commands.json for environment: {}...",
            env_label
        );
    }
    let (caller_pid, caller_cwd) = daemon_client::caller_info();
    let req = BuildRequest {
        project_dir: project_dir.clone(),
        environment,
        clean_build: clean,
        verbose,
        jobs,
        profile,
        generate_compiledb,
        request_id: None,
        caller_pid,
        caller_cwd,
    };

    let resp = client.build(&req).await?;
    println!("{}", resp.message);
    if !resp.success {
        std::process::exit(resp.exit_code);
    }
    if generate_compiledb {
        let db_path = std::path::Path::new(&project_dir).join("compile_commands.json");
        if db_path.exists() {
            println!("compile_commands.json written to {}", db_path.display());
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_deploy(
    project_dir: String,
    environment: Option<String>,
    port: Option<String>,
    clean: bool,
    monitor_after: bool,
    verbose: bool,
    timeout: Option<f64>,
    halt_on_error: Option<String>,
    halt_on_success: Option<String>,
    expect: Option<String>,
    no_timestamp: bool,
    skip_build: bool,
    qemu: bool,
    qemu_timeout: u32,
) -> fbuild_core::Result<()> {
    if qemu {
        return Err(fbuild_core::FbuildError::Other(
            "QEMU deployment is not yet supported in the Rust port. Use --platformio for QEMU support.".into(),
        ));
    }
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

    let (caller_pid, caller_cwd) = daemon_client::caller_info();
    let req = DeployRequest {
        project_dir,
        environment,
        port,
        monitor_after,
        skip_build,
        clean_build: clean,
        verbose,
        monitor_timeout: timeout,
        monitor_halt_on_error: halt_on_error,
        monitor_halt_on_success: halt_on_success,
        monitor_expect: expect,
        monitor_show_timestamp: !no_timestamp,
        qemu,
        qemu_timeout,
        request_id: None,
        caller_pid,
        caller_cwd,
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
    no_timestamp: bool,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

    let (caller_pid, caller_cwd) = daemon_client::caller_info();
    let req = MonitorRequest {
        project_dir,
        environment,
        port,
        baud_rate,
        halt_on_error,
        halt_on_success,
        expect,
        timeout,
        show_timestamp: !no_timestamp,
        request_id: None,
        caller_pid,
        caller_cwd,
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

    match target.as_deref() {
        None => {
            // No target: list cached packages (matches Python behavior)
            list_cached_packages(&cache_root)?;
            std::process::exit(1);
        }
        Some("all") => {
            // Purge entire global cache
            purge_dir(&cache_root, dry_run)?;
        }
        Some("project") => {
            // Purge project-local .fbuild/ directory
            let pd = project_dir.as_deref().unwrap_or(".");
            let fbuild_dir = fbuild_paths::get_project_fbuild_dir(std::path::Path::new(pd));
            purge_dir(&fbuild_dir, dry_run)?;
        }
        Some(t) => {
            // Purge specific cache subdirectory (e.g., environment name)
            let path = cache_root.join(t);
            if !path.exists() {
                eprintln!("target not found: {}", path.display());
                return Ok(());
            }
            purge_dir(&path, dry_run)?;
        }
    }
    Ok(())
}

fn purge_dir(path: &std::path::Path, dry_run: bool) -> fbuild_core::Result<()> {
    if !path.exists() {
        println!("nothing to purge: {}", path.display());
        return Ok(());
    }
    let size = dir_size(path);
    if dry_run {
        println!("would remove: {} ({})", path.display(), format_size(size));
    } else {
        std::fs::remove_dir_all(path).map_err(|e| {
            fbuild_core::FbuildError::Other(format!("failed to remove {}: {}", path.display(), e))
        })?;
        println!("removed: {} ({})", path.display(), format_size(size));
    }
    Ok(())
}

fn list_cached_packages(cache_root: &std::path::Path) -> fbuild_core::Result<()> {
    if !cache_root.exists() {
        println!("No cached packages found at {}", cache_root.display());
        println!("\nUsage:");
        println!("  fbuild purge all              Remove all cached packages");
        println!("  fbuild purge project          Remove project build artifacts (.fbuild/)");
        println!("  fbuild purge <name>           Remove specific cache subdirectory");
        println!("  fbuild purge ... --dry-run    Show what would be removed");
        return Ok(());
    }

    let mut total_size: u64 = 0;
    let mut total_count: usize = 0;

    // Walk top-level type directories (toolchains, platforms, frameworks, etc.)
    let mut entries: Vec<_> = std::fs::read_dir(cache_root)
        .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to read cache dir: {}", e)))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for type_entry in entries {
        let type_name = type_entry.file_name();
        let type_path = type_entry.path();

        // Collect packages within this type directory
        let mut packages: Vec<(String, u64)> = Vec::new();
        if let Ok(subdirs) = std::fs::read_dir(&type_path) {
            for sub in subdirs.filter_map(|e| e.ok()) {
                let sub_path = sub.path();
                if sub_path.is_dir() {
                    let name = sub.file_name().to_string_lossy().to_string();
                    let size = dir_size(&sub_path);
                    packages.push((name, size));
                }
            }
        }
        packages.sort_by(|a, b| a.0.cmp(&b.0));

        if !packages.is_empty() {
            println!("{}:", type_name.to_string_lossy().to_uppercase());
            for (name, size) in &packages {
                println!("  {} ({})", name, format_size(*size));
                total_size += size;
                total_count += 1;
            }
            println!();
        }
    }

    println!(
        "Total: {} package(s), {}",
        total_count,
        format_size(total_size)
    );
    println!("\nUse 'fbuild purge all' to remove all, or 'fbuild purge <target>' for specific.");
    Ok(())
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut size = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.is_dir() {
                size += dir_size(&p);
            } else if let Ok(meta) = p.metadata() {
                size += meta.len();
            }
        }
    }
    size
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

async fn run_device(action: DeviceAction) -> fbuild_core::Result<()> {
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

async fn run_daemon(action: DaemonAction) -> fbuild_core::Result<()> {
    let client = DaemonClient::new();
    match action {
        DaemonAction::Stop => {
            if !client.health().await {
                println!("daemon is not running");
                return Ok(());
            }
            client.shutdown().await?;
            // Wait for it to actually stop
            for _ in 0..50 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if !client.health().await {
                    println!("daemon stopped");
                    return Ok(());
                }
            }
            println!("daemon stop requested (may still be shutting down)");
        }
        DaemonAction::Status => {
            if client.health().await {
                match client.daemon_info().await {
                    Ok(info) => {
                        let uptime = format_uptime(info.uptime_seconds);
                        println!("daemon is running at {}", fbuild_paths::get_daemon_url());
                        println!("  PID:     {}", info.pid);
                        println!("  Port:    {}", info.port);
                        println!("  Uptime:  {}", uptime);
                        println!("  Version: {}", info.version);
                        println!("  Mode:    {}", if info.dev_mode { "dev" } else { "prod" });
                        println!("  State:   {}", info.daemon_state);
                        if info.operation_in_progress {
                            if let Some(ref op) = info.current_operation {
                                println!("  Operation: {}", op);
                            } else {
                                println!("  Operation: (in progress)");
                            }
                        }
                        if info.client_count > 0 {
                            println!("  Clients: {}", info.client_count);
                        }
                        if let Some(ref cwd) = info.spawner_cwd {
                            println!("  Spawned from: {}", cwd);
                        }
                        if let Some(mtime) = info.source_mtime {
                            println!("  Binary mtime: {:.0}", mtime);
                        }
                    }
                    Err(_) => {
                        println!("daemon is running at {}", fbuild_paths::get_daemon_url());
                    }
                }
            } else {
                println!("daemon is not running");
            }
        }
        DaemonAction::Restart => {
            // Stop if running
            if client.health().await {
                client.shutdown().await?;
                for _ in 0..50 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    if !client.health().await {
                        break;
                    }
                }
            }
            // Start fresh
            daemon_client::ensure_daemon_running().await?;
            println!("daemon restarted");
        }
        DaemonAction::List => {
            run_daemon_list(&client).await?;
        }
        DaemonAction::Kill { pid, force } => {
            run_daemon_kill(&client, pid, force).await?;
        }
        DaemonAction::KillAll { force } => {
            run_daemon_kill_all(force)?;
        }
        DaemonAction::Locks => {
            run_daemon_locks(&client).await?;
        }
        DaemonAction::ClearLocks => {
            run_daemon_clear_locks(&client).await?;
        }
        DaemonAction::Monitor { no_follow, lines } => {
            return run_show("daemon", !no_follow, lines);
        }
    }
    Ok(())
}

async fn run_daemon_list(client: &DaemonClient) -> fbuild_core::Result<()> {
    if client.health().await {
        match client.daemon_info().await {
            Ok(info) => {
                let uptime = format_uptime(info.uptime_seconds);
                println!("fbuild daemon (running)");
                println!("  PID:     {}", info.pid);
                println!("  Port:    {}", info.port);
                println!("  Uptime:  {}", uptime);
                println!("  Version: {}", info.version);
                println!("  Mode:    {}", if info.dev_mode { "dev" } else { "prod" });
            }
            Err(e) => {
                println!("daemon is running but info unavailable: {}", e);
            }
        }
    } else {
        println!("no daemon is running");
        // Check for stale PID file
        let pid_file = fbuild_paths::get_daemon_pid_file();
        if pid_file.exists() {
            if let Ok(contents) = std::fs::read_to_string(&pid_file) {
                println!(
                    "  (stale PID file: {} — PID {})",
                    pid_file.display(),
                    contents.trim()
                );
            }
        }
    }

    // Also scan for orphan processes
    let pids = find_daemon_pids()?;
    if pids.len() > 1 {
        println!("\nwarning: multiple fbuild-daemon processes detected:");
        for pid in &pids {
            println!("  PID {}", pid);
        }
        println!("use 'fbuild daemon kill-all' to clean up");
    }
    Ok(())
}

async fn run_daemon_locks(client: &DaemonClient) -> fbuild_core::Result<()> {
    if !client.health().await {
        println!("daemon is not running");
        return Ok(());
    }

    let status = client.lock_status().await?;

    // Display port locks
    if status.port_locks.is_empty() {
        println!("Port Locks: (none)");
    } else {
        println!("Port Locks:");
        for lock in &status.port_locks {
            let state = if lock.is_held { "HELD" } else { "FREE" };
            let writer = lock.writer_client_id.as_deref().unwrap_or("none");
            println!(
                "  {} [{}] open={} writer={} readers={}",
                lock.port, state, lock.is_open, writer, lock.reader_count
            );
        }
    }

    // Display project locks
    if status.project_locks.is_empty() {
        println!("Project Locks: (none)");
    } else {
        println!("Project Locks:");
        for lock in &status.project_locks {
            let state = if lock.is_held { "HELD" } else { "FREE" };
            println!("  {} [{}]", lock.project_dir, state);
        }
    }

    if !status.stale_locks.is_empty() {
        println!(
            "\nWarning: {} stale lock(s) detected. Use 'fbuild daemon clear-locks' to clear.",
            status.stale_locks.len()
        );
    }

    Ok(())
}

async fn run_daemon_clear_locks(client: &DaemonClient) -> fbuild_core::Result<()> {
    if !client.health().await {
        println!("daemon is not running");
        return Ok(());
    }

    let result = client.clear_locks().await?;
    println!("{}", result.message);
    if result.cleared_count > 0 {
        println!("Cleared {} lock(s)", result.cleared_count);
    }
    Ok(())
}

async fn run_daemon_kill(
    client: &DaemonClient,
    pid: Option<u32>,
    force: bool,
) -> fbuild_core::Result<()> {
    let target_pid = if let Some(p) = pid {
        p
    } else if client.health().await {
        match client.daemon_info().await {
            Ok(info) => info.pid,
            Err(_) => read_pid_from_file()?,
        }
    } else {
        read_pid_from_file()?
    };

    kill_process(target_pid, force)?;
    println!("killed daemon (PID {})", target_pid);
    let _ = std::fs::remove_file(fbuild_paths::get_daemon_pid_file());
    Ok(())
}

fn read_pid_from_file() -> fbuild_core::Result<u32> {
    let pid_file = fbuild_paths::get_daemon_pid_file();
    if pid_file.exists() {
        std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| {
                fbuild_core::FbuildError::DaemonError(
                    "could not parse PID from PID file".to_string(),
                )
            })
    } else {
        Err(fbuild_core::FbuildError::DaemonError(
            "no daemon running and no PID file found".to_string(),
        ))
    }
}

fn run_daemon_kill_all(force: bool) -> fbuild_core::Result<()> {
    let pids = find_daemon_pids()?;
    if pids.is_empty() {
        println!("no fbuild-daemon processes found");
        return Ok(());
    }

    let mut killed = 0;
    for pid in &pids {
        match kill_process(*pid, force) {
            Ok(()) => {
                println!("killed daemon (PID {})", pid);
                killed += 1;
            }
            Err(e) => {
                eprintln!("failed to kill PID {}: {}", pid, e);
            }
        }
    }

    let _ = std::fs::remove_file(fbuild_paths::get_daemon_pid_file());
    println!("killed {} daemon(s)", killed);
    Ok(())
}

fn kill_process(pid: u32, force: bool) -> fbuild_core::Result<()> {
    let output = if cfg!(windows) {
        let mut cmd = std::process::Command::new("taskkill");
        if force {
            cmd.arg("/F");
        }
        cmd.arg("/PID").arg(pid.to_string());
        cmd.output()
    } else {
        let signal = if force { "-9" } else { "-TERM" };
        std::process::Command::new("kill")
            .arg(signal)
            .arg(pid.to_string())
            .output()
    };

    let output = output.map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to execute kill command: {}", e))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(fbuild_core::FbuildError::Other(format!(
            "kill failed: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

fn find_daemon_pids() -> fbuild_core::Result<Vec<u32>> {
    if cfg!(windows) {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq fbuild-daemon.exe", "/FO", "CSV", "/NH"])
            .output()
            .map_err(|e| {
                fbuild_core::FbuildError::Other(format!("failed to run tasklist: {}", e))
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut pids = Vec::new();
        for line in stdout.lines() {
            // CSV format: "image name","PID","session name","session#","mem usage"
            if line.contains("fbuild-daemon") {
                let fields: Vec<&str> = line.split(',').collect();
                if fields.len() >= 2 {
                    let pid_str = fields[1].trim_matches('"').trim();
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        pids.push(pid);
                    }
                }
            }
        }
        Ok(pids)
    } else {
        let output = std::process::Command::new("pgrep")
            .args(["-f", "fbuild-daemon"])
            .output()
            .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to run pgrep: {}", e)))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let pids: Vec<u32> = stdout
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect();
        Ok(pids)
    }
}

fn format_uptime(seconds: f64) -> String {
    let secs = seconds as u64;
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn run_show(target: &str, follow: bool, lines: usize) -> fbuild_core::Result<()> {
    match target {
        "daemon" => show_daemon_logs(follow, lines),
        other => {
            eprintln!("unknown show target: '{}' (available: daemon)", other);
            std::process::exit(1);
        }
    }
}

fn show_daemon_logs(follow: bool, initial_lines: usize) -> fbuild_core::Result<()> {
    let log_path = fbuild_paths::get_daemon_log_file();
    if !log_path.exists() {
        eprintln!("daemon log file not found: {}", log_path.display());
        eprintln!("the daemon may not have been started yet");
        return Ok(());
    }

    let content = std::fs::read_to_string(&log_path)
        .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to read log file: {}", e)))?;

    // Show last N lines
    let all_lines: Vec<&str> = content.lines().collect();
    let start = all_lines.len().saturating_sub(initial_lines);
    for line in &all_lines[start..] {
        println!("{}", line);
    }

    if !follow {
        return Ok(());
    }

    // Follow mode: poll for new content
    println!("--- following {} (Ctrl+C to stop) ---", log_path.display());
    let mut pos = content.len() as u64;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let current_len = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(pos);

        if current_len > pos {
            use std::io::{Read, Seek};
            if let Ok(mut file) = std::fs::File::open(&log_path) {
                let _ = file.seek(std::io::SeekFrom::Start(pos));
                let mut buf = String::new();
                if file.read_to_string(&mut buf).is_ok() && !buf.is_empty() {
                    print!("{}", buf);
                }
                pos = current_len;
            }
        } else if current_len < pos {
            // Log file was truncated/rotated — re-read from start
            pos = 0;
        }
    }
}

fn run_reset(
    project_dir: String,
    environment: Option<String>,
    port: Option<String>,
    verbose: bool,
) -> fbuild_core::Result<()> {
    let project_path = std::path::Path::new(&project_dir);
    let ini_path = project_path.join("platformio.ini");

    // Read config to detect board/platform
    let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
    let env_name = if let Some(ref e) = environment {
        e.clone()
    } else {
        config
            .get_default_environment()
            .ok_or_else(|| {
                fbuild_core::FbuildError::ConfigError(
                    "no environment found in platformio.ini".to_string(),
                )
            })?
            .to_string()
    };

    let env_config = config.get_env_config(&env_name)?;
    let board = env_config.get("board").ok_or_else(|| {
        fbuild_core::FbuildError::ConfigError(format!(
            "no 'board' key in environment '{}'",
            env_name
        ))
    })?;

    let platform = fbuild_deploy::reset::detect_platform_for_reset(board);

    // Determine port
    let port = port.ok_or_else(|| {
        fbuild_core::FbuildError::SerialError("no serial port specified (use --port)".to_string())
    })?;

    println!("resetting {} device on {}...", platform, port);
    match fbuild_deploy::reset::reset_device(platform, &port, verbose)? {
        true => {
            println!("device reset successful");
            Ok(())
        }
        false => {
            eprintln!("device reset failed");
            std::process::exit(1);
        }
    }
}
