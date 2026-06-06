//! Clap-derived CLI parser, subcommand enums, and small argv helpers.
//!
//! Carved out of `main.rs` so the parser definition can live alongside
//! `KNOWN_SUBCOMMANDS` / `rewrite_args` / `resolve_project_dir` without
//! pulling in the handler bodies.

use clap::{Parser, Subcommand};

use super::monitor_parse::parse_jobs;

#[derive(Parser)]
#[command(
    name = "fbuild",
    version,
    about = "PlatformIO-compatible embedded build tool"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Project directory (positional, for `fbuild <dir>`)
    pub project_dir: Option<String>,

    /// Target environment
    #[arg(short = 'e', long)]
    pub environment: Option<String>,

    /// Verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// Serial port (e.g., COM5, /dev/ttyUSB0)
    #[arg(short = 'p', long)]
    pub port: Option<String>,

    /// Clean build before deploy
    #[arg(short = 'c', long)]
    pub clean: bool,

    /// Monitor after deploy; optionally pass flags as a string
    #[arg(long, num_args = 0..=1, default_missing_value = "")]
    pub monitor: Option<String>,

    /// Use PlatformIO compatibility mode
    #[arg(long)]
    pub platformio: bool,

    /// Monitor timeout in seconds
    #[arg(long)]
    pub timeout: Option<f64>,

    /// Halt monitor on error pattern
    #[arg(long)]
    pub halt_on_error: Option<String>,

    /// Halt monitor on success pattern
    #[arg(long)]
    pub halt_on_success: Option<String>,

    /// Expected output pattern for monitor
    #[arg(long)]
    pub expect: Option<String>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Fine-grained per-symbol bloat analysis of an existing ELF.
    ///
    /// Runs `nm --print-size --size-sort -S` on the ELF, demangles via
    /// `c++filt`, parses the alongside linker map (auto-detected as
    /// `<elf-stem>.map` or `firmware.map`), and emits a table that
    /// attributes each live symbol to its source archive + object +
    /// output section. Useful for diffing two builds at the symbol
    /// level. JSON output is suitable for scripted comparison.
    Symbols {
        /// Path to the ELF to analyze.
        elf: String,
        /// Path to the linker map (auto-detected if omitted).
        #[arg(long)]
        map: Option<String>,
        /// Path to `nm` (auto-detected from PATH if omitted; pass the
        /// cross-tool, e.g. `xtensa-esp32s3-elf-nm`).
        #[arg(long)]
        nm: Option<String>,
        /// Path to `c++filt` (auto-derived from `nm` path if omitted).
        #[arg(long = "cppfilt")]
        cppfilt: Option<String>,
        /// Write structured report to PATH as JSON instead of the text
        /// table.
        #[arg(long)]
        json: Option<String>,
        /// Number of top symbols / archives to show in the text report.
        #[arg(long, default_value = "25")]
        top: usize,
    },
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
    /// Manage `.lnk` resource pointers (fetch / verify / add).
    ///
    /// `.lnk` files are tiny JSON manifests checked into source control
    /// that point at remote binary blobs (sha256-verified). At build time
    /// fbuild downloads + caches them; this command lets you operate on
    /// them outside of a build.
    Lnk {
        #[command(subcommand)]
        action: LnkAction,
    },
    /// Diagnostic: drive the LDF-style library-selection resolver and print
    /// the selected library set. Useful for debugging FastLED/fbuild#202 /
    /// `#204`-style "library not found" issues without running a full build.
    LibSelect {
        /// Project directory (defaults to ".").
        project_dir: Option<String>,
        /// Target environment.
        #[arg(short = 'e', long)]
        environment: Option<String>,
        /// Show selection origin per library, unresolved headers, etc.
        #[arg(long, conflicts_with = "json")]
        explain: bool,
        /// Emit machine-readable JSON instead of plain text.
        #[arg(long, conflicts_with = "explain")]
        json: bool,
    },
    /// Two-stage compile of many sketches against the same board
    /// (FastLED/fbuild#238). Builds the framework + library archives once
    /// (stage 1) and fans out per-sketch compile + link in parallel
    /// (stage 2). Independent parallelism knobs for each stage so memory-
    /// heavy framework work stays modest while sketch work saturates cores.
    CompileMany {
        /// Board id (e.g. "uno", "teensy41"). Used to dispatch to the
        /// right platform orchestrator and to pick the matching
        /// `[env:<board>]` (or first env with `board = <board>`) inside
        /// each sketch's `platformio.ini`.
        #[arg(long)]
        board: String,
        /// Parallelism for stage 1 (framework + library compile). When
        /// omitted, defaults to `min(cores, 2)`.
        #[arg(long)]
        framework_jobs: Option<usize>,
        /// Parallelism for stage 2 (per-sketch compile + link). When
        /// omitted, defaults to `cores`.
        #[arg(long)]
        sketch_jobs: Option<usize>,
        /// Build profile.
        #[arg(long, group = "compile_many_profile")]
        quick: bool,
        #[arg(long, group = "compile_many_profile")]
        release: bool,
        /// Verbose compiler output.
        #[arg(short, long)]
        verbose: bool,
        /// Emit JSONL per stage-2 worker with seed/build timing.
        #[arg(long)]
        diag_stage2: bool,
        /// Sketch project directories (each must contain `platformio.ini`).
        #[arg(required = true)]
        sketches: Vec<String>,
    },
    /// PlatformIO-compatible CI command (FastLED/fbuild#242). Drop-in
    /// replacement for `pio ci` that delegates to the `compile-many`
    /// two-stage primitive (FastLED/fbuild#241).
    Ci {
        /// Board id (e.g. "uno", "teensy41"). Matches `pio ci --board`.
        #[arg(short = 'b', long)]
        board: String,
        /// Extra library search directory (repeatable). Mapped to the
        /// `PLATFORMIO_LIB_EXTRA_DIRS` env var, ';' separated on Windows
        /// and ':' separated elsewhere. Matches `pio ci --lib`.
        #[arg(short = 'l', long = "lib")]
        libs: Vec<String>,
        /// Path to a `platformio.ini` to use instead of the per-sketch
        /// one. Mapped to `PLATFORMIO_PROJECT_CONFIG`. Matches
        /// `pio ci --project-conf`.
        #[arg(short = 'c', long = "project-conf")]
        project_conf: Option<String>,
        /// Accepted for compatibility with `pio ci`; build directories
        /// are always kept under `.fbuild/build/...` (no-op).
        #[arg(long)]
        keep_build_dir: bool,
        /// Accepted for compatibility with `pio ci`. Not yet honored;
        /// emits a warning when set.
        #[arg(long)]
        build_dir: Option<String>,
        /// Parallelism for stage 1 (framework + library compile).
        #[arg(long)]
        framework_jobs: Option<usize>,
        /// Parallelism for stage 2 (per-sketch compile + link).
        #[arg(long)]
        sketch_jobs: Option<usize>,
        /// Build profile.
        #[arg(long, group = "ci_profile")]
        quick: bool,
        #[arg(long, group = "ci_profile")]
        release: bool,
        /// Verbose compiler output.
        #[arg(short, long)]
        verbose: bool,
        /// Emit JSONL per stage-2 worker with seed/build timing.
        #[arg(long)]
        diag_stage2: bool,
        /// Sketches to build. Each entry is either a project directory
        /// containing `platformio.ini` or a `.ino` file whose parent
        /// directory is the project. Matches `pio ci` positional args.
        #[arg(required = true)]
        sketches: Vec<String>,
    },
}

/// Subcommands for `fbuild lnk`.
#[derive(Subcommand)]
pub enum LnkAction {
    /// Walk the current dir (or a project root) and fetch every `.lnk`
    /// referenced blob into the disk cache. Cache hits are no-ops.
    Pull {
        /// Project root to scan. Defaults to the current directory.
        project_dir: Option<String>,
    },
    /// Verify every `.lnk` blob in the cache matches its sha256, without
    /// touching the network. Reports mismatches; exits non-zero on any.
    Check {
        /// Project root to scan. Defaults to the current directory.
        project_dir: Option<String>,
    },
    /// Download a URL once, compute its sha256, and write a new `.lnk`
    /// JSON pointing at it. Useful for adding new resources without
    /// hand-editing JSON.
    Add {
        /// URL to download.
        url: String,
        /// Where to write the `.lnk` file. Defaults to the URL's basename
        /// + `.lnk` in the current directory.
        #[arg(short = 'o', long)]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum DeviceAction {
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
pub enum DaemonAction {
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
pub fn resolve_project_dir(
    subcommand_dir: Option<String>,
    top_level_dir: &Option<String>,
) -> String {
    subcommand_dir
        .or_else(|| top_level_dir.clone())
        .unwrap_or_else(|| ".".to_string())
}

/// Known subcommand names for arg rewriting.
pub const KNOWN_SUBCOMMANDS: &[&str] = &[
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
    "lib-select",
    "compile-many",
    "ci",
];

/// Rewrite `fbuild <dir> <subcommand> ...` → `fbuild <subcommand> <dir> ...`
/// so that both `fbuild build <dir>` and `fbuild <dir> build` work.
pub fn rewrite_args() -> Vec<String> {
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
