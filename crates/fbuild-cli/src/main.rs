mod daemon_client;
mod mcp;

use clap::{Parser, Subcommand};
use daemon_client::{BuildRequest, DaemonClient, DeployRequest, MonitorRequest, TestEmuRequest};

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
        project_dir: Option<String>,
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
        /// Run per-symbol memory analysis after building; optionally write report to PATH
        /// instead of streaming to console
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        symbol_analysis: Option<String>,
        /// Disable elapsed-time prefix on build output lines
        #[arg(long)]
        no_timestamp: bool,
        /// Export build artifacts to a tooling-friendly directory
        #[arg(long)]
        output_dir: Option<String>,
    },
    /// Deploy firmware to device
    Deploy {
        project_dir: Option<String>,
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
        /// Deploy to the native QEMU emulator instead of a physical device
        #[arg(long)]
        qemu: bool,
        /// Timeout in seconds for QEMU execution (default: 30)
        #[arg(long, default_value = "30")]
        qemu_timeout: u32,
        /// Override the board's default upload baud rate
        #[arg(short = 'b', long = "baud", alias = "baud-rate")]
        baud_rate: Option<u32>,
        /// Deploy destination: device (default) or emulator
        #[arg(long = "to", value_parser = ["device", "emu", "emulator"])]
        to: Option<String>,
        /// Emulator backend when deploying to `emu`
        #[arg(long, value_parser = ["avr8js", "qemu", "simavr"])]
        emulator: Option<String>,
        /// Legacy deploy target alias: device, qemu, or avr8js
        #[arg(long, value_parser = ["device", "qemu", "avr8js"], hide = true)]
        target: Option<String>,
        /// Export build artifacts to a tooling-friendly directory
        #[arg(long)]
        output_dir: Option<String>,
    },
    /// Monitor serial output
    Monitor {
        project_dir: Option<String>,
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
        /// Run LRU garbage collection instead of full purge
        #[arg(long)]
        gc: bool,
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
    /// Run clang-tidy static analysis on project sources
    ClangTidy {
        project_dir: Option<String>,
        #[arg(short = 'e', long)]
        environment: Option<String>,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Run include-what-you-use analysis on project sources
    Iwyu {
        project_dir: Option<String>,
        #[arg(short = 'e', long)]
        environment: Option<String>,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Build firmware and run it in an emulator for testing
    TestEmu {
        project_dir: Option<String>,
        #[arg(short = 'e', long)]
        environment: Option<String>,
        #[arg(short, long)]
        verbose: bool,
        /// Timeout in seconds for the emulator run
        #[arg(long)]
        timeout: Option<f64>,
        /// Halt on error pattern (regex)
        #[arg(long)]
        halt_on_error: Option<String>,
        /// Halt on success pattern (regex)
        #[arg(long)]
        halt_on_success: Option<String>,
        /// Expected output pattern (regex)
        #[arg(long)]
        expect: Option<String>,
        /// Disable timestamp prefix on output lines
        #[arg(long)]
        no_timestamp: bool,
        /// Emulator backend: "qemu", "avr8js", or "simavr" (auto-detected if omitted)
        #[arg(long, value_parser = ["avr8js", "qemu", "simavr"])]
        emulator: Option<String>,
    },
    /// Run clang-query on project sources
    ClangQuery {
        project_dir: Option<String>,
        #[arg(short = 'e', long)]
        environment: Option<String>,
        #[arg(short, long)]
        verbose: bool,
        /// clang-query matcher expression
        #[arg(short = 'm', long)]
        matcher: Option<String>,
    },
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
    /// Show disk cache statistics
    CacheStats,
    /// Run disk cache garbage collection
    Gc,
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

/// Resolve project_dir: prefer the subcommand's value, fall back to the top-level positional arg,
/// then default to ".".  This lets callers write either `fbuild build <dir>` or `fbuild <dir> build`.
fn resolve_project_dir(subcommand_dir: Option<String>, top_level_dir: &Option<String>) -> String {
    subcommand_dir
        .or_else(|| top_level_dir.clone())
        .unwrap_or_else(|| ".".to_string())
}

/// Known subcommand names for arg rewriting.
const KNOWN_SUBCOMMANDS: &[&str] = &[
    "build",
    "deploy",
    "monitor",
    "reset",
    "purge",
    "show",
    "daemon",
    "device",
    "mcp",
    "clang-tidy",
    "iwyu",
    "clang-query",
    "test-emu",
];

/// Rewrite `fbuild <dir> <subcommand> ...` → `fbuild <subcommand> <dir> ...`
/// so that both `fbuild build <dir>` and `fbuild <dir> build` work.
fn rewrite_args() -> Vec<String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 {
        let first = &args[1];
        let second = &args[2];
        // If first arg is NOT a subcommand and second IS, swap them
        if !first.starts_with('-')
            && !KNOWN_SUBCOMMANDS.contains(&first.as_str())
            && KNOWN_SUBCOMMANDS.contains(&second.as_str())
        {
            let mut rewritten = Vec::with_capacity(args.len());
            rewritten.push(args[0].clone());
            rewritten.push(second.clone()); // subcommand first
            rewritten.push(first.clone()); // project_dir second
            rewritten.extend(args[3..].iter().cloned());
            return rewritten;
        }
    }
    args
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse_from(rewrite_args());

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Scan caller's environment for `PLATFORMIO_*` vars and warn about any
    // that fbuild does not act on, so users aren't bitten by silent
    // mis-builds. The captured map is forwarded to the daemon per request.
    let pio_env = daemon_client::capture_pio_env();
    for var in fbuild_config::scan_unsupported(&pio_env) {
        tracing::warn!("{} is set but not supported by fbuild (ignored)", var);
    }
    for var in fbuild_config::scan_warn_only(&pio_env) {
        tracing::warn!("{} is set but fbuild does not act on it", var);
    }

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

    // Extract top-level project_dir before matching (since match partially moves cli)
    let top_level_project_dir = cli.project_dir.clone();

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
            symbol_analysis,
            no_timestamp,
            output_dir,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
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
                    symbol_analysis,
                    no_timestamp,
                    output_dir,
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
            baud_rate,
            to,
            emulator,
            target,
            output_dir,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
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
                    baud_rate,
                    to,
                    emulator,
                    target,
                    output_dir,
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
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
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
            gc,
        }) => {
            if gc {
                run_purge_gc().await
            } else {
                run_purge(target, dry_run, project_dir)
            }
        }
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
        Some(Commands::ClangTidy {
            project_dir,
            environment,
            verbose,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            run_clang_tool(
                fbuild_packages::toolchain::ClangComponentKind::ClangExtra,
                "clang-tidy",
                project_dir,
                environment,
                verbose,
                &[],
            )
            .await
        }
        Some(Commands::Iwyu {
            project_dir,
            environment,
            verbose,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            run_iwyu(project_dir, environment, verbose).await
        }
        Some(Commands::TestEmu {
            project_dir,
            environment,
            verbose,
            timeout,
            halt_on_error,
            halt_on_success,
            expect,
            no_timestamp,
            emulator,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            run_test_emu(
                project_dir,
                environment,
                verbose,
                timeout,
                halt_on_error,
                halt_on_success,
                expect,
                no_timestamp,
                emulator,
            )
            .await
        }
        Some(Commands::ClangQuery {
            project_dir,
            environment,
            verbose,
            matcher,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            let extra: Vec<String> = matcher
                .map(|m| vec!["-c".to_string(), m])
                .unwrap_or_default();
            let extra_refs: Vec<&str> = extra.iter().map(|s| s.as_str()).collect();
            run_clang_tool(
                fbuild_packages::toolchain::ClangComponentKind::ClangExtra,
                "clang-query",
                project_dir,
                environment,
                verbose,
                &extra_refs,
            )
            .await
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
                let monitor_after = true;
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
                    None,
                    None,
                    None,
                    None,
                    None,
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

fn open_in_browser(url: &str) -> fbuild_core::Result<()> {
    let status = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .status()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    }
    .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to launch browser: {}", e)))?;

    if status.success() {
        Ok(())
    } else {
        Err(fbuild_core::FbuildError::Other(format!(
            "browser launcher exited with status {}",
            status
        )))
    }
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
    symbol_analysis: Option<String>,
    no_timestamp: bool,
    output_dir: Option<String>,
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
        compiledb_only: generate_compiledb,
        request_id: None,
        caller_pid,
        caller_cwd,
        stream: true,
        symbol_analysis: symbol_analysis.is_some(),
        symbol_analysis_path: symbol_analysis.filter(|s| !s.is_empty()),
        no_timestamp,
        src_dir: std::env::var("PLATFORMIO_SRC_DIR")
            .ok()
            .filter(|s| !s.is_empty()),
        output_dir,
        pio_env: daemon_client::capture_pio_env(),
    };

    let resp = client.build_streaming(&req).await?;
    if !resp.message.is_empty() {
        println!("{}", resp.message);
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CliEmulatorKind {
    Qemu,
    Avr8js,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CliDeployRoute {
    Device,
    Emulator(CliEmulatorKind),
}

fn infer_cli_default_emulator_kind(
    project_dir: &str,
    environment: Option<&str>,
) -> fbuild_core::Result<Option<CliEmulatorKind>> {
    let project_dir = std::path::Path::new(project_dir);
    let config = fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))
        .map_err(|e| {
            fbuild_core::FbuildError::Other(format!("failed to parse platformio.ini: {}", e))
        })?;
    let env_name = environment
        .map(|s| s.to_string())
        .or_else(|| config.get_default_environment().map(|s| s.to_string()))
        .unwrap_or_else(|| "default".to_string());
    let env_config = config.get_env_config(&env_name).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("invalid environment '{}': {}", env_name, e))
    })?;
    let platform_str = env_config.get("platform").cloned().unwrap_or_default();
    let Some(platform) = fbuild_core::Platform::from_platform_str(&platform_str) else {
        return Ok(None);
    };
    let Some(board_id) = env_config.get("board").cloned() else {
        return Ok(None);
    };
    let board_overrides = config.get_board_overrides(&env_name).unwrap_or_default();
    let board = fbuild_config::BoardConfig::from_board_id(&board_id, &board_overrides)
        .or_else(|_| {
            fbuild_config::BoardConfig::from_board_id(&board_id, &std::collections::HashMap::new())
        })
        .ok();
    Ok(
        match (platform, board.as_ref().map(|board| board.mcu.as_str())) {
            (fbuild_core::Platform::AtmelAvr, _) | (fbuild_core::Platform::AtmelMegaAvr, _) => {
                Some(CliEmulatorKind::Avr8js)
            }
            (fbuild_core::Platform::Espressif32, Some(mcu))
                if mcu.eq_ignore_ascii_case("esp32s3") =>
            {
                Some(CliEmulatorKind::Qemu)
            }
            _ => None,
        },
    )
}

fn resolve_cli_deploy_route(
    to: Option<&str>,
    emulator: Option<&str>,
    target: Option<&str>,
    qemu: bool,
    default_emulator: Option<CliEmulatorKind>,
) -> fbuild_core::Result<CliDeployRoute> {
    if let Some(target) = target {
        return match target {
            "device" => Ok(CliDeployRoute::Device),
            "qemu" => Ok(CliDeployRoute::Emulator(CliEmulatorKind::Qemu)),
            "avr8js" => Ok(CliDeployRoute::Emulator(CliEmulatorKind::Avr8js)),
            other => Err(fbuild_core::FbuildError::Other(format!(
                "unsupported deploy target '{}'",
                other
            ))),
        };
    }

    match to.unwrap_or("device") {
        "device" => {
            if qemu {
                return Err(fbuild_core::FbuildError::Other(
                    "--qemu cannot be combined with --to device".to_string(),
                ));
            }
            if let Some(emulator) = emulator {
                return Err(fbuild_core::FbuildError::Other(format!(
                    "--emulator {} requires --to emu",
                    emulator
                )));
            }
            Ok(CliDeployRoute::Device)
        }
        "emu" | "emulator" => {
            let emulator = if qemu {
                if let Some(explicit) = emulator {
                    if explicit != "qemu" {
                        return Err(fbuild_core::FbuildError::Other(
                            "--qemu cannot be combined with a different --emulator".to_string(),
                        ));
                    }
                }
                "qemu"
            } else {
                match emulator {
                    Some(explicit) => explicit,
                    None => match default_emulator {
                        Some(CliEmulatorKind::Qemu) => "qemu",
                        Some(CliEmulatorKind::Avr8js) => "avr8js",
                        None => {
                            return Err(fbuild_core::FbuildError::Other(
                                "--to emu requires an explicit --emulator for this board"
                                    .to_string(),
                            ))
                        }
                    },
                }
            };
            match emulator {
                "qemu" => Ok(CliDeployRoute::Emulator(CliEmulatorKind::Qemu)),
                "avr8js" => Ok(CliDeployRoute::Emulator(CliEmulatorKind::Avr8js)),
                other => Err(fbuild_core::FbuildError::Other(format!(
                    "unsupported emulator '{}'",
                    other
                ))),
            }
        }
        other => Err(fbuild_core::FbuildError::Other(format!(
            "unsupported deploy destination '{}'",
            other
        ))),
    }
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
    baud_rate: Option<u32>,
    to: Option<String>,
    emulator: Option<String>,
    target: Option<String>,
    output_dir: Option<String>,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

    let default_emulator = if matches!(to.as_deref(), Some("emu" | "emulator"))
        && emulator.is_none()
        && target.is_none()
        && !qemu
    {
        infer_cli_default_emulator_kind(&project_dir, environment.as_deref())?
    } else {
        None
    };
    let deploy_route = resolve_cli_deploy_route(
        to.as_deref(),
        emulator.as_deref(),
        target.as_deref(),
        qemu,
        default_emulator,
    )?;

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
        baud_rate,
        to,
        emulator,
        target,
        qemu,
        qemu_timeout,
        request_id: None,
        caller_pid,
        caller_cwd,
        src_dir: std::env::var("PLATFORMIO_SRC_DIR")
            .ok()
            .filter(|s| !s.is_empty()),
        output_dir,
        pio_env: daemon_client::capture_pio_env(),
    };

    let resp = client.deploy(&req).await?;
    if deploy_route == CliDeployRoute::Emulator(CliEmulatorKind::Qemu)
        || deploy_route == CliDeployRoute::Emulator(CliEmulatorKind::Avr8js)
    {
        print_operation_streams(&resp);
    }
    println!("{}", resp.message);
    if !resp.success {
        std::process::exit(resp.exit_code);
    }
    // Open browser for avr8js only when daemon returned a launch URL (non-headless mode)
    if deploy_route == CliDeployRoute::Emulator(CliEmulatorKind::Avr8js) {
        if let Some(url) = resp.launch_url.as_deref() {
            if let Err(e) = open_in_browser(url) {
                eprintln!("warning: failed to open browser: {}", e);
                eprintln!("open this URL manually: {}", url);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_test_emu(
    project_dir: String,
    environment: Option<String>,
    verbose: bool,
    timeout: Option<f64>,
    halt_on_error: Option<String>,
    halt_on_success: Option<String>,
    expect: Option<String>,
    no_timestamp: bool,
    emulator: Option<String>,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

    let (caller_pid, caller_cwd) = daemon_client::caller_info();
    let req = TestEmuRequest {
        project_dir,
        environment,
        verbose,
        timeout,
        halt_on_error,
        halt_on_success,
        expect,
        emulator,
        show_timestamp: !no_timestamp,
        request_id: None,
        caller_pid,
        caller_cwd,
        pio_env: daemon_client::capture_pio_env(),
    };

    let resp = client.test_emu(&req).await?;
    print_operation_streams(&resp);
    println!("{}", resp.message);
    if !resp.success {
        std::process::exit(resp.exit_code);
    }
    Ok(())
}

fn print_operation_streams(resp: &daemon_client::OperationResponse) {
    if let Some(stdout) = resp
        .stdout
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        print!("{}", stdout);
        if !stdout.ends_with('\n') {
            println!();
        }
    }
    if let Some(stderr) = resp
        .stderr
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        eprint!("{}", stderr);
        if !stderr.ends_with('\n') {
            eprintln!();
        }
    }
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

/// Convert MSYS/Git-Bash paths (/c/Users/...) to native Windows paths and canonicalize.
fn normalize_path(path: &str) -> fbuild_core::Result<String> {
    let converted = if cfg!(windows) {
        // /c/foo → C:\foo
        let bytes = path.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[2] == b'/'
            && bytes[1].is_ascii_alphabetic()
        {
            let drive = (bytes[1] as char).to_ascii_uppercase();
            format!("{}:{}", drive, path[2..].replace('/', "\\"))
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    };
    let canon = std::fs::canonicalize(&converted).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("cannot resolve path '{}': {}", path, e))
    })?;
    let s = canon.to_string_lossy().to_string();
    // Strip \\?\ prefix that canonicalize adds on Windows
    Ok(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

/// Run IWYU (include-what-you-use) analysis with ESP32 cross-compilation support.
///
/// Unlike the generic `run_clang_tool()`, this:
/// 1. Downloads IWYU via ClangComponent
/// 2. Generates compile_commands.json via the fbuild daemon
/// 3. Preprocesses the database: adds GCC builtin includes, converts framework
///    `-I` to `-isystem`, removes `--target=` flags, deduplicates `-D` defines
/// 4. Writes a modified compile_commands.json to a temp dir
/// 5. Runs IWYU per-source-file with `-p <temp_dir>`
/// 6. Filters output to only show suggestions for files under `src/`
async fn run_iwyu(
    project_dir: String,
    environment: Option<String>,
    verbose: bool,
) -> fbuild_core::Result<()> {
    let project_dir = normalize_path(&project_dir)?;

    // Step 1: Ensure IWYU is installed
    let component = fbuild_packages::toolchain::ClangComponent::new(
        fbuild_packages::toolchain::ClangComponentKind::Iwyu,
    );
    let tool_path = component.get_binary("include-what-you-use").await?;
    println!("Using include-what-you-use: {}", tool_path.display());

    // Step 2: Generate compile_commands.json via fbuild daemon (skip if it already exists)
    let project_path = std::path::Path::new(&project_dir);
    let db_path = project_path.join("compile_commands.json");
    if db_path.exists() {
        println!("Using existing compile_commands.json");
    } else {
        println!("Generating compile_commands.json...");
        run_build(
            project_dir.clone(),
            environment.clone(),
            false,
            verbose,
            None,
            false,
            false,
            false,
            Some("compiledb".to_string()),
            None,
            true, // no_timestamp: compiledb generation doesn't need timestamps
            None,
        )
        .await?;
        if !db_path.exists() {
            return Err(fbuild_core::FbuildError::Other(
                "compile_commands.json was not generated".into(),
            ));
        }
    }
    let db_content = std::fs::read_to_string(&db_path).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to read compile_commands.json: {}", e))
    })?;
    let entries: Vec<serde_json::Value> = serde_json::from_str(&db_content).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to parse compile_commands.json: {}", e))
    })?;

    // Filter to project source files only.
    // Source files can be under <project>/src/ (original) or
    // <project>/.fbuild/build/<env>/*/src/ (build copies with Arduino preprocessing).
    // Exclude framework/SDK files (paths containing cache/platforms/ or cache/toolchains/).
    let src_dir = project_path.join("src");
    let build_src_suffix = format!("{}src", std::path::MAIN_SEPARATOR_STR);
    let project_prefix = project_path
        .to_string_lossy()
        .replace('/', std::path::MAIN_SEPARATOR_STR);
    let source_entries: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|e| {
            e.get("file")
                .and_then(|f| f.as_str())
                .map(|f| {
                    let p = std::path::Path::new(f);
                    // Direct match: file is under <project>/src/
                    if p.starts_with(&src_dir) {
                        return true;
                    }
                    // Build copy: file is under <project>/.fbuild/build/.../src/
                    let f_normalized = f.replace('/', std::path::MAIN_SEPARATOR_STR);
                    f_normalized.starts_with(&project_prefix)
                        && f_normalized.contains(&build_src_suffix)
                        && !f_normalized.contains("cache")
                })
                .unwrap_or(false)
        })
        .collect();

    if source_entries.is_empty() {
        println!("No source files found in compile_commands.json under src/");
        return Ok(());
    }

    // Step 4: Find GCC toolchain builtin include dirs
    let gcc_includes = fbuild_packages::toolchain::clang::find_gcc_builtin_include_dirs();
    if !gcc_includes.is_empty() {
        println!("Found {} GCC builtin include dir(s)", gcc_includes.len());
        if verbose {
            for inc in &gcc_includes {
                println!("  {}", inc.display());
            }
        }
    }

    // Step 5: Preprocess compile_commands.json for IWYU
    // Transform entries directly as JSON: remove --target=, dedup -D, convert -I to -isystem
    let src_prefix = src_dir.to_string_lossy().replace('\\', "/").to_lowercase();
    let iwyu_entries: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            let mut new_entry = entry.clone();
            if let Some(args) = entry.get("arguments").and_then(|a| a.as_array()) {
                let mut new_args: Vec<serde_json::Value> =
                    Vec::with_capacity(args.len() + gcc_includes.len() * 2);
                let mut seen_defines = std::collections::HashSet::new();

                for arg_val in args {
                    let arg = arg_val.as_str().unwrap_or("");

                    // Remove --target= flags
                    if arg.starts_with("--target=") {
                        continue;
                    }

                    // Remove GCC-only flags unsupported by IWYU's clang
                    if matches!(
                        arg,
                        "-freorder-blocks"
                            | "-fno-jump-tables"
                            | "-flto"
                            | "-flto=auto"
                            | "-fno-fat-lto-objects"
                            | "-fuse-linker-plugin"
                            | "-ffat-lto-objects"
                            | "-mlongcalls"
                            | "-mdisable-hardware-atomics"
                            | "-fstrict-volatile-bitfields"
                            | "-mtext-section-literals"
                            | "-fno-tree-switch-conversion"
                            | "-mthumb-interwork"
                    ) || arg.starts_with("-mfix-esp32-psram-cache-strategy=")
                    {
                        continue;
                    }

                    // Deduplicate -D flags (keep first occurrence by key)
                    if arg.starts_with("-D") {
                        let key = if let Some(eq_pos) = arg.find('=') {
                            &arg[..eq_pos]
                        } else {
                            arg
                        };
                        if !seen_defines.insert(key.to_string()) {
                            continue;
                        }
                    }

                    // Convert non-project -I to -isystem
                    if let Some(path) = arg.strip_prefix("-I") {
                        let normalized = path.replace('\\', "/").to_lowercase();
                        if normalized.starts_with(&src_prefix) {
                            new_args.push(arg_val.clone());
                        } else {
                            new_args.push(serde_json::Value::String("-isystem".into()));
                            new_args.push(serde_json::Value::String(path.to_string()));
                        }
                        continue;
                    }

                    new_args.push(arg_val.clone());
                }

                // Append GCC toolchain builtin include dirs as -isystem
                for inc in &gcc_includes {
                    new_args.push(serde_json::Value::String("-isystem".into()));
                    new_args.push(serde_json::Value::String(inc.to_string_lossy().to_string()));
                }

                new_entry["arguments"] = serde_json::Value::Array(new_args);
            }
            new_entry
        })
        .collect();

    // Write modified compile_commands.json to .fbuild/iwyu/ for IWYU to read via -p
    let iwyu_dir_path = fbuild_paths::get_project_fbuild_dir(project_path).join("iwyu");
    std::fs::create_dir_all(&iwyu_dir_path).map_err(|e| {
        fbuild_core::FbuildError::Other(format!(
            "failed to create {}: {}",
            iwyu_dir_path.display(),
            e
        ))
    })?;
    let iwyu_db_path = iwyu_dir_path.join("compile_commands.json");
    let iwyu_json = serde_json::to_string_pretty(&iwyu_entries).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to serialize IWYU compile database: {}", e))
    })?;
    std::fs::write(&iwyu_db_path, iwyu_json).map_err(|e| {
        fbuild_core::FbuildError::Other(format!(
            "failed to write {}: {}",
            iwyu_db_path.display(),
            e
        ))
    })?;
    // Step 6: Set up zccache-style content-addressed cache for IWYU results.
    // Cache key = blake3(source_content + iwyu_entry_json) per file.
    let cache_dir = iwyu_dir_path.join("cache");
    std::fs::create_dir_all(&cache_dir).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to create {}: {}", cache_dir.display(), e))
    })?;

    // Build a lookup from file path → preprocessed IWYU entry JSON for cache keying.
    let iwyu_entry_map: std::collections::HashMap<String, String> = iwyu_entries
        .iter()
        .filter_map(|e| {
            let file = e.get("file")?.as_str()?.to_string();
            let json = serde_json::to_string(e).ok()?;
            Some((file, json))
        })
        .collect();

    println!(
        "Running include-what-you-use on {} source file(s)...",
        source_entries.len()
    );

    // Step 7: Run IWYU in parallel with caching
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get() * 2)
        .unwrap_or(4);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(jobs));
    let tool_arc = std::sync::Arc::new(tool_path);
    let iwyu_dir = std::sync::Arc::new(iwyu_dir_path);
    let src_dir_arc = std::sync::Arc::new(src_dir.clone());
    let cache_dir_arc = std::sync::Arc::new(cache_dir);
    let entry_map_arc = std::sync::Arc::new(iwyu_entry_map);

    // Collect source file paths from the filtered entries
    let source_files: Vec<String> = source_entries
        .iter()
        .filter_map(|e| e.get("file").and_then(|f| f.as_str()).map(String::from))
        .collect();

    let mut handles = Vec::new();
    for file in source_files {
        let sem = semaphore.clone();
        let tool = tool_arc.clone();
        let p_dir = iwyu_dir.clone();
        let src = src_dir_arc.clone();
        let cache_d = cache_dir_arc.clone();
        let emap = entry_map_arc.clone();
        let verbose_flag = verbose;
        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let src_path = src.as_ref().clone();

            // Compute blake3 cache key from source content + compile entry
            let cache_key = iwyu_cache_key(&file, &emap);
            let cache_path: Option<std::path::PathBuf> = cache_key
                .as_ref()
                .map(|k| cache_d.join(format!("{}.txt", k)));

            // Check cache
            if let Some(ref cp) = cache_path {
                if cp.exists() {
                    if let Ok(cached) = std::fs::read_to_string(cp) {
                        return (file, Ok(cached), true, src_path);
                    }
                }
            }

            // Cache miss — run IWYU
            let mut cmd = tokio::process::Command::new(tool.as_ref());
            cmd.arg("-p").arg(p_dir.as_ref());
            cmd.arg("-Xiwyu").arg("--no_comments");
            cmd.arg("-Xiwyu").arg("--quoted_includes_first");
            cmd.arg("-Xiwyu").arg("--max_line_length=100");
            if verbose_flag {
                cmd.arg("-Xiwyu").arg("--verbose=3");
            }
            cmd.arg(&file);
            let output = cmd.output().await;

            match output {
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                    // Store in cache on success
                    if let Some(ref cp) = cache_path {
                        let _ = std::fs::write(cp, &stderr);
                    }
                    (file, Ok(stderr), false, src_path)
                }
                Err(e) => (file, Err(format!("{}", e)), false, src_path),
            }
        });
        handles.push(handle);
    }

    let mut total_suggestions = 0usize;
    let mut failed_files = Vec::new();
    let mut cache_hits = 0usize;
    let mut cache_misses = 0usize;

    for handle in handles {
        let (file, result, cached, src_path) = handle
            .await
            .map_err(|e| fbuild_core::FbuildError::Other(format!("task join error: {}", e)))?;
        if cached {
            cache_hits += 1;
        } else {
            cache_misses += 1;
        }
        match result {
            Ok(stderr) => {
                let filtered = filter_iwyu_output(&stderr, &src_path);
                if !filtered.trim().is_empty() {
                    print!("{}", filtered);
                    total_suggestions += filtered
                        .lines()
                        .filter(|l| l.contains("should add") || l.contains("should remove"))
                        .count();
                }
            }
            Err(e) => {
                eprintln!("failed to run include-what-you-use on {}: {}", file, e);
                failed_files.push(file);
            }
        }
    }

    println!("\n--- include-what-you-use summary ---");
    println!("Suggestions: {}", total_suggestions);
    println!(
        "Cache:       {} hit(s), {} miss(es)",
        cache_hits, cache_misses
    );
    if !failed_files.is_empty() {
        println!("Failed:      {} file(s)", failed_files.len());
    }

    if !failed_files.is_empty() {
        Err(fbuild_core::FbuildError::BuildFailed(
            "include-what-you-use failed on some files".into(),
        ))
    } else {
        Ok(())
    }
}

/// Compute a blake3 cache key for an IWYU analysis of a source file.
///
/// The key is derived from the source file content and the preprocessed
/// compile_commands.json entry for that file (which includes all flags).
/// This mirrors zccache's content-addressed hashing strategy.
fn iwyu_cache_key(
    file: &str,
    entry_map: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let source_content = std::fs::read(file).ok()?;
    let entry_json = entry_map.get(file)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"fbuild-iwyu-cache-v1");
    hasher.update(&source_content);
    hasher.update(entry_json.as_bytes());
    Some(hasher.finalize().to_hex().to_string())
}

/// Filter IWYU output to show only suggestions for files under `src_dir`.
///
/// IWYU outputs blocks like:
/// ```text
/// /path/to/file.h should add these lines:
/// #include <foo.h>
///
/// /path/to/file.h should remove these lines:
/// - #include <bar.h>
///
/// The full include-list for /path/to/file.h:
/// #include <baz.h>
/// ---
/// ```
///
/// We only keep blocks whose file path is under `src_dir`.
fn filter_iwyu_output(output: &str, src_dir: &std::path::Path) -> String {
    let src_prefix = src_dir.to_string_lossy().replace('\\', "/").to_lowercase();
    let mut result = String::new();
    let mut current_block = String::new();
    let mut block_is_user_file = false;

    for line in output.lines() {
        // Detect block headers: "path/to/file should add/remove these lines:"
        // or "The full include-list for path/to/file:"
        let is_header = line.contains(" should add these lines")
            || line.contains(" should remove these lines")
            || line.starts_with("The full include-list for ");

        if is_header {
            // Flush previous block if it was a user file
            if block_is_user_file && !current_block.trim().is_empty() {
                result.push_str(&current_block);
                result.push('\n');
            }
            current_block.clear();

            // Check if this new block's file is under src_dir
            let file_path = line
                .split(" should ")
                .next()
                .or_else(|| {
                    line.strip_prefix("The full include-list for ")
                        .and_then(|s| s.strip_suffix(':'))
                })
                .unwrap_or("");
            let normalized = file_path.replace('\\', "/").to_lowercase();
            block_is_user_file = normalized.starts_with(&src_prefix);
        }

        current_block.push_str(line);
        current_block.push('\n');
    }

    // Flush last block
    if block_is_user_file && !current_block.trim().is_empty() {
        result.push_str(&current_block);
    }

    result
}

/// Generic runner for clang-based analysis tools (clang-tidy, clang-query).
///
/// 1. Ensure tool binary is installed via ClangComponent (downloads on demand)
/// 2. Generate compile_commands.json via fbuild daemon (build -t compiledb)
/// 3. Run tool on each source file in parallel (ncpus * 2)
#[allow(clippy::too_many_arguments)]
async fn run_clang_tool(
    kind: fbuild_packages::toolchain::ClangComponentKind,
    binary_name: &str,
    project_dir: String,
    environment: Option<String>,
    verbose: bool,
    extra_args: &[&str],
) -> fbuild_core::Result<()> {
    let project_dir = normalize_path(&project_dir)?;

    // Step 1: Ensure tool is installed
    let component = fbuild_packages::toolchain::ClangComponent::new(kind);
    let tool_path = component.get_binary(binary_name).await?;
    println!("Using {}: {}", binary_name, tool_path.display());

    // Step 2: Generate compile_commands.json via fbuild daemon
    println!("Generating compile_commands.json...");
    run_build(
        project_dir.clone(),
        environment.clone(),
        false, // clean
        verbose,
        None,  // jobs
        false, // quick
        false, // release
        false, // dry_run
        Some("compiledb".to_string()),
        None,
        true, // no_timestamp: compiledb generation doesn't need timestamps
        None,
    )
    .await?;

    // Step 3: Read compile_commands.json to get source files
    let project_path = std::path::Path::new(&project_dir);
    let db_path = project_path.join("compile_commands.json");
    if !db_path.exists() {
        return Err(fbuild_core::FbuildError::Other(
            "compile_commands.json was not generated".into(),
        ));
    }
    let db_content = std::fs::read_to_string(&db_path).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to read compile_commands.json: {}", e))
    })?;
    let entries: Vec<serde_json::Value> = serde_json::from_str(&db_content).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to parse compile_commands.json: {}", e))
    })?;

    // Filter to project source files only (under src/)
    let src_dir = project_path.join("src");
    let source_files: Vec<String> = entries
        .iter()
        .filter_map(|e| e.get("file").and_then(|f| f.as_str()).map(String::from))
        .filter(|f| {
            let p = std::path::Path::new(f);
            p.starts_with(&src_dir)
        })
        .collect();

    if source_files.is_empty() {
        println!("No source files found in compile_commands.json under src/");
        return Ok(());
    }
    println!(
        "Running {} on {} source file(s)...",
        binary_name,
        source_files.len()
    );

    // Step 4: Run tool in parallel with ncpus * 2
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get() * 2)
        .unwrap_or(4);
    println!("Using {} parallel jobs", jobs);

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(jobs));
    let project_dir_arc = std::sync::Arc::new(project_dir.clone());
    let tool_arc = std::sync::Arc::new(tool_path);
    let extra_owned: Vec<String> = extra_args.iter().map(|s| s.to_string()).collect();
    let verbose_flag = verbose;

    let mut handles = Vec::new();
    for file in source_files {
        let sem = semaphore.clone();
        let tool = tool_arc.clone();
        let pd = project_dir_arc.clone();
        let extra = extra_owned.clone();
        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let mut cmd = tokio::process::Command::new(tool.as_ref());
            cmd.arg("-p").arg(pd.as_ref());
            for arg in &extra {
                cmd.arg(arg);
            }
            cmd.arg(&file);
            if !verbose_flag {
                cmd.arg("--quiet");
            }
            let output = cmd.output().await;
            (file, output)
        });
        handles.push(handle);
    }

    let mut total_warnings = 0usize;
    let mut total_errors = 0usize;
    let mut failed_files = Vec::new();

    for handle in handles {
        let (file, result) = handle
            .await
            .map_err(|e| fbuild_core::FbuildError::Other(format!("task join error: {}", e)))?;
        match result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let combined = format!("{}{}", stdout, stderr);
                if !combined.trim().is_empty() {
                    print!("{}", combined);
                }
                for line in combined.lines() {
                    if line.contains("warning:") {
                        total_warnings += 1;
                    }
                    if line.contains("error:") {
                        total_errors += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("failed to run {} on {}: {}", binary_name, file, e);
                failed_files.push(file);
            }
        }
    }

    println!("\n--- {} summary ---", binary_name);
    println!("Warnings: {}", total_warnings);
    println!("Errors:   {}", total_errors);
    if !failed_files.is_empty() {
        println!("Failed:   {} file(s)", failed_files.len());
    }

    if total_errors > 0 || !failed_files.is_empty() {
        Err(fbuild_core::FbuildError::BuildFailed(format!(
            "{} found errors",
            binary_name
        )))
    } else {
        Ok(())
    }
}

async fn run_purge_gc() -> fbuild_core::Result<()> {
    // Try to route GC through the daemon to respect its gc_mutex.
    let client = DaemonClient::new();
    if client.health().await {
        let result = client.run_gc().await?;
        if !result.success {
            return Err(fbuild_core::FbuildError::Other(format!(
                "GC failed: {}",
                result.message.as_deref().unwrap_or("unknown error")
            )));
        }
        println!("GC complete (via daemon):");
        println!(
            "  Installed evicted: {} ({})",
            result.installed_evicted,
            format_size(result.installed_bytes_freed)
        );
        println!(
            "  Archives evicted:  {} ({})",
            result.archives_evicted,
            format_size(result.archive_bytes_freed)
        );
        println!(
            "  Total freed:       {}",
            format_size(result.total_bytes_freed)
        );
        if result.orphan_files_removed > 0 {
            println!("  Orphan files removed: {}", result.orphan_files_removed);
        }
        if result.orphan_rows_cleaned > 0 {
            println!("  Orphan rows cleaned:  {}", result.orphan_rows_cleaned);
        }
        return Ok(());
    }

    // No daemon running — safe to run GC locally.
    match fbuild_packages::DiskCache::open() {
        Ok(dc) => match dc.run_gc() {
            Ok(report) => {
                print_gc_report(&report);
                Ok(())
            }
            Err(e) => Err(fbuild_core::FbuildError::Other(format!("GC failed: {}", e))),
        },
        Err(e) => Err(fbuild_core::FbuildError::Other(format!(
            "failed to open disk cache: {}",
            e
        ))),
    }
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
        DaemonAction::CacheStats => {
            run_daemon_cache_stats(&client).await?;
        }
        DaemonAction::Gc => {
            run_daemon_gc(&client).await?;
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

async fn run_daemon_cache_stats(client: &DaemonClient) -> fbuild_core::Result<()> {
    if !client.health().await {
        // Fall back to local cache stats if daemon isn't running
        match fbuild_packages::DiskCache::open() {
            Ok(dc) => {
                let stats = dc.stats().map_err(|e| {
                    fbuild_core::FbuildError::Other(format!("failed to read cache stats: {}", e))
                })?;
                println!("{}", stats);
            }
            Err(e) => {
                return Err(fbuild_core::FbuildError::Other(format!(
                    "failed to open disk cache: {}",
                    e
                )));
            }
        }
        return Ok(());
    }

    let stats = client.cache_stats().await?;
    if !stats.success {
        return Err(fbuild_core::FbuildError::Other(format!(
            "failed to get cache stats: {}",
            stats.message.as_deref().unwrap_or("unknown error")
        )));
    }
    println!("Disk Cache Statistics:");
    println!("  Entries:    {}", stats.entry_count);
    println!("  Installed:  {}", format_size(stats.installed_bytes));
    println!("  Archives:   {}", format_size(stats.archive_bytes));
    println!("  Total:      {}", format_size(stats.total_bytes));
    println!(
        "  Watermarks: {} high / {} low",
        format_size(stats.high_watermark),
        format_size(stats.low_watermark)
    );
    println!("  Archive budget: {}", format_size(stats.archive_budget));
    Ok(())
}

async fn run_daemon_gc(client: &DaemonClient) -> fbuild_core::Result<()> {
    if !client.health().await {
        // Fall back to local GC if daemon isn't running
        let dc = fbuild_packages::DiskCache::open().map_err(|e| {
            fbuild_core::FbuildError::Other(format!("failed to open disk cache: {}", e))
        })?;
        let report = dc
            .run_gc()
            .map_err(|e| fbuild_core::FbuildError::Other(format!("GC failed: {}", e)))?;
        print_gc_report(&report);
        return Ok(());
    }

    let result = client.run_gc().await?;
    if !result.success {
        return Err(fbuild_core::FbuildError::Other(format!(
            "GC failed: {}",
            result.message.as_deref().unwrap_or("unknown error")
        )));
    }
    println!("GC complete:");
    println!(
        "  Installed evicted: {} ({})",
        result.installed_evicted,
        format_size(result.installed_bytes_freed)
    );
    println!(
        "  Archives evicted:  {} ({})",
        result.archives_evicted,
        format_size(result.archive_bytes_freed)
    );
    println!(
        "  Total freed:       {}",
        format_size(result.total_bytes_freed)
    );
    if result.orphan_files_removed > 0 {
        println!("  Orphan files removed: {}", result.orphan_files_removed);
    }
    if result.orphan_rows_cleaned > 0 {
        println!("  Orphan rows cleaned:  {}", result.orphan_rows_cleaned);
    }
    Ok(())
}

fn print_gc_report(report: &fbuild_packages::disk_cache::GcReport) {
    if report.total_bytes_freed() == 0
        && report.orphan_files_removed == 0
        && report.orphan_rows_cleaned == 0
    {
        println!("GC: nothing to clean up");
        return;
    }
    println!("GC complete:");
    println!(
        "  Installed evicted: {} ({})",
        report.installed_evicted,
        format_size(report.installed_bytes_freed)
    );
    println!(
        "  Archives evicted:  {} ({})",
        report.archives_evicted,
        format_size(report.archive_bytes_freed)
    );
    println!(
        "  Total freed:       {}",
        format_size(report.total_bytes_freed())
    );
    if report.orphan_files_removed > 0 {
        println!("  Orphan files removed: {}", report.orphan_files_removed);
    }
    if report.orphan_rows_cleaned > 0 {
        println!("  Orphan rows cleaned:  {}", report.orphan_rows_cleaned);
    }
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
