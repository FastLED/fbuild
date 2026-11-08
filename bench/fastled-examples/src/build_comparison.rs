//! Nightly Arduino CLI vs PlatformIO vs fbuild whole-build benchmark.
//!
//! The harness measures the same Arduino Uno and ESP32-S3 Blink sketch with each real CLI,
//! then renders the one-commit benchmark site's JSON, SVG, and HTML artifacts.

#[path = "pio_phases.rs"]
mod pio_phases;

use fbuild_core::path::NormalizedPath;
use fbuild_paths::get_project_fbuild_dir;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const HISTORY_MAX_LINES: usize = 365;
const PERF_LOG_FILE: &str = "fbuild-perf.jsonl";
/// Prefix of the CLI's per-command daemon restart notice (FastLED/fbuild#1476).
const DAEMON_RESTART_NOTICE: &str = "daemon binary updated, restarting";
/// Lines of the daemon's own log appended to `benchmark.log` on restart mode.
const DAEMON_LOG_TAIL_LINES: usize = 200;
const PERF_PHASE_LABELS: &[&str] = &["avr-orchestrator", "esp32-orchestrator", "pipeline"];
/// A cold build on the shaped link that fetched less than this reused an
/// installed package, so its timing is not a cold measurement.
const MIN_COLD_DOWNLOAD_BYTES: u64 = 1_000_000;
const COLD_INSTALL_ATTEMPTS: usize = 3;
const REGRESSION_WINDOW_S: u64 = 7 * 24 * 60 * 60;
const DEFAULT_REPOSITORY: &str = "FastLED/fbuild";
const DEFAULT_PAGES_URL: &str = "https://fastled.github.io/fbuild/";
const DEFAULT_RAW_BASE_URL: &str =
    "https://raw.githubusercontent.com/FastLED/fbuild/benchmark-stats";

#[derive(Clone, Copy, Debug)]
struct Board {
    key: &'static str,
    name: &'static str,
    environment: &'static str,
    fqbn: &'static str,
    /// Exact `arduino-cli core install` spec, run inside the timed cold window
    /// because `arduino-cli compile` does not install missing cores.
    arduino_core: &'static str,
}

const BOARDS: [Board; 2] = [
    Board {
        key: "uno",
        name: "Arduino Uno",
        environment: "uno",
        fqbn: "arduino:avr:uno",
        arduino_core: "arduino:avr@1.8.8",
    },
    Board {
        key: "esp32s3",
        name: "ESP32-S3",
        environment: "esp32s3",
        fqbn: "esp32:esp32:esp32s3",
        arduino_core: "esp32:esp32@3.3.12",
    },
];

type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

fn tool_envs(kind: ToolKind, perf_log: &Path) -> Vec<(&'static str, OsString)> {
    if matches!(kind, ToolKind::Fbuild) {
        vec![
            ("FBUILD_PERF_LOG", OsString::from("1")),
            ("FBUILD_PERF_LOG_JSON", perf_log.as_os_str().to_os_string()),
            // Include the mtime comparison in benchmark.log if a run hits the
            // intermittent restart path (FastLED/fbuild#1476).
            ("RUST_LOG", OsString::from("fbuild=info")),
        ]
    } else {
        Vec::new()
    }
}

#[derive(Debug)]
struct Options {
    output_dir: NormalizedPath,
    project_dir: NormalizedPath,
    fbuild: NormalizedPath,
    arduino_cli: OsString,
    platformio: OsString,
    trials: usize,
    /// Link rate the runner was shaped to, in Mbit/s. 0 means unthrottled.
    network_mbit: u32,
    repository: String,
    git_sha: String,
    generated_at: String,
    run_url: String,
    pages_url: String,
    raw_base_url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolKind {
    Arduino,
    PlatformIo,
    Fbuild,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ColdCleanupStep {
    Command {
        program: OsString,
        args: Vec<OsString>,
    },
    RemoveDir(NormalizedPath),
    /// Remove every immediate child of `parent` whose name starts with
    /// `prefix`, never following symlinks. Used for package/home caches that
    /// live outside the repository, where `remove_dir_within` does not apply.
    RemoveCacheEntries {
        parent: NormalizedPath,
        prefix: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MeasurementStep {
    PrepareCold(usize),
    ColdBuild(usize),
    WarmBuild(usize),
}

#[derive(Clone, Copy, Debug)]
struct ToolStyle {
    key: &'static str,
    label: &'static str,
    warm_color: &'static str,
}

impl ToolKind {
    fn style(self) -> ToolStyle {
        match self {
            Self::Arduino => ToolStyle {
                key: "arduino",
                label: "Arduino CLI",
                warm_color: "#8b949e",
            },
            Self::PlatformIo => ToolStyle {
                key: "platformio",
                label: "PlatformIO",
                warm_color: "#79c0ff",
            },
            Self::Fbuild => ToolStyle {
                key: "fbuild",
                label: "fbuild",
                warm_color: "#f85149",
            },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct ToolResult {
    board: String,
    board_name: String,
    tool: String,
    display_name: String,
    version: String,
    cold_ms: f64,
    warm_ms: f64,
    speedup: f64,
    cold_trials_ms: Vec<f64>,
    warm_trials_ms: Vec<f64>,
    /// Median bytes received during a cold build; `None` when unmeasured.
    cold_download_bytes: Option<u64>,
    cold_download_trials_bytes: Vec<u64>,
    /// Per-phase cold medians: fbuild's perf log, or PlatformIO's `-v` output
    /// split by `pio_phases` (empty for other tools).
    cold_phases_ms: BTreeMap<String, f64>,
    /// Raw per-trial phase timings for each cold build (fbuild and PlatformIO).
    cold_phase_trials: Vec<BTreeMap<String, f64>>,
    /// Timed fbuild builds that restarted the daemon (FastLED/fbuild#1476);
    /// each adds ~200 ms, so a non-zero count marks inflated fbuild timings.
    daemon_restarts: usize,
    /// Package versions actually selected by this tool, not requested pins.
    resolved_packages: BTreeMap<String, String>,
    /// Why package identity was unavailable, if a successful build omitted it.
    #[serde(skip_serializing_if = "Option::is_none")]
    package_metadata_warning: Option<String>,
    /// Distinct Arduino core sources in the tool's compile database.
    core_source_count: Option<usize>,
    /// Full compiler invocation for a representative core source.
    core_compile_argv: Option<Vec<String>>,
}

/// One entry of a clang-style `compile_commands.json`.
#[derive(Clone, Debug, Deserialize)]
struct CompileEntry {
    directory: String,
    #[serde(default)]
    file: String,
    #[serde(default)]
    arguments: Option<Vec<String>>,
    #[serde(default)]
    command: Option<String>,
}

#[derive(Clone, Debug)]
struct Metadata {
    generated_at: String,
    git_sha: String,
    repository: String,
    run_url: String,
    project: String,
    trials: usize,
    /// Link rate the runner was shaped to, in Mbit/s. 0 means unthrottled.
    network_mbit: u32,
    /// Per-board median wall clock of replaying the bare compiler.
    raw_baselines_ms: BTreeMap<String, f64>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("build comparison benchmark failed: {error}");
        std::process::exit(1);
    }
}

#[expect(clippy::too_many_lines, reason = "baseline, zackees/ci.yml#229")]
fn run() -> AppResult<()> {
    let options = parse_options()?;
    let repo_root = env::current_dir()?;
    let project_dir = absolute_from(&repo_root, &options.project_dir);
    let output_dir = absolute_from(&repo_root, &options.output_dir);
    let fbuild = absolute_from(&repo_root, &options.fbuild);

    if !project_dir.join("blink.ino").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not contain blink.ino", project_dir.display()),
        )
        .into());
    }
    if !fbuild.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("fbuild binary not found at {}", fbuild.display()),
        )
        .into());
    }

    fs::create_dir_all(&output_dir)?;
    let log_dir = repo_root.join("benchmark-output");
    fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join("benchmark.log");
    let mut log = File::create(&log_path)?;

    let versions = [
        (
            ToolKind::Arduino,
            command_version(&options.arduino_cli, &["version"])?,
        ),
        (
            ToolKind::PlatformIo,
            command_version(&options.platformio, &["--version"])?,
        ),
        (
            ToolKind::Fbuild,
            command_version(fbuild.as_os_str(), &["--version"])?,
        ),
    ];

    let mut results = Vec::with_capacity(versions.len() * BOARDS.len());
    let mut raw_baselines_ms = BTreeMap::new();
    for board in BOARDS {
        let arduino_build_dir = repo_root.join(format!("benchmark-output/arduino-{}", board.key));
        for (kind, version) in &versions {
            let result = measure_tool(
                *kind,
                version,
                board,
                &options,
                &repo_root,
                &project_dir,
                &output_dir,
                &fbuild,
                &arduino_build_dir,
                &mut log,
                &mut raw_baselines_ms,
            )?;
            if result.daemon_restarts > 0 {
                println!(
                    "::warning title=fbuild daemon restart mode::{} timed fbuild builds restarted the daemon (~200 ms each), so fbuild timings are inflated; the restart notices and daemon.log tail are in benchmark.log (FastLED/fbuild#1476)",
                    result.daemon_restarts
                );
                append_daemon_log_tail(&mut log)?;
            }
            println!(
                "{:<11} {:<12} cold {:>10.3} ms | warm {:>10.3} ms | {:>7.2}x",
                board.name, result.display_name, result.cold_ms, result.warm_ms, result.speedup
            );
            results.push(result);
        }
    }

    let metadata = Metadata {
        generated_at: options.generated_at.clone(),
        git_sha: options.git_sha.clone(),
        repository: options.repository.clone(),
        run_url: options.run_url.clone(),
        project: options.project_dir.display_slash(),
        trials: options.trials,
        network_mbit: options.network_mbit,
        raw_baselines_ms,
    };
    let history = read_history_values(&output_dir.join("history.jsonl"));
    let now_unix_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    for board in BOARDS {
        if let Some(ratio) = board_cold_ratio(&results, board.key) {
            let Some(baseline) = ratio_regressed_for_board(&history, now_unix_s, ratio, board.key)
            else {
                continue;
            };
            println!(
                "::warning title=fbuild cold regression ({})::fbuild/PlatformIO cold ratio {ratio:.3} exceeds 7-day median {baseline:.3}",
                board.name
            );
        }
    }
    write_outputs(
        &output_dir,
        &metadata,
        &results,
        &options.pages_url,
        &options.raw_base_url,
    )?;
    println!("Published benchmark artifacts to {}", output_dir.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[expect(clippy::too_many_lines, reason = "baseline, zackees/ci.yml#229")]
fn measure_tool(
    kind: ToolKind,
    version: &str,
    board: Board,
    options: &Options,
    repo_root: &Path,
    project_dir: &Path,
    output_dir: &Path,
    fbuild: &Path,
    arduino_build_dir: &Path,
    log: &mut File,
    raw_baselines_ms: &mut BTreeMap<String, f64>,
) -> AppResult<ToolResult> {
    let mut cold_trials_ms = Vec::with_capacity(options.trials);
    let mut warm_trials_ms = Vec::with_capacity(options.trials);
    let mut cold_phase_trials = Vec::new();
    let mut cold_download_trials_bytes = Vec::new();
    let mut daemon_restarts = 0;
    let mut resolved_packages = BTreeMap::new();
    let mut package_metadata_warning = None;
    let perf_log = output_dir.join(PERF_LOG_FILE);
    let envs = tool_envs(kind, &perf_log);

    if matches!(kind, ToolKind::Fbuild) {
        writeln!(log, "\n===== fbuild daemon preflight =====")?;
        run_logged_env(
            fbuild.as_os_str(),
            &os_args(&["daemon", "restart"]),
            repo_root,
            log,
            &envs,
        )?;
    }

    for step in measurement_plan(options.trials) {
        match step {
            MeasurementStep::PrepareCold(trial) => {
                writeln!(
                    log,
                    "\n===== {} trial {trial}/{} =====",
                    kind.style().label,
                    options.trials
                )?;
                prepare_cold(
                    kind,
                    board,
                    options,
                    repo_root,
                    project_dir,
                    fbuild,
                    arduino_build_dir,
                    &envs,
                    log,
                )?;
            }
            MeasurementStep::ColdBuild(trial) => {
                let offset = perf_line_count(&perf_log);
                let rx_before = network_rx_bytes();
                let install_ms = timed_cold_install(kind, board, options, repo_root, log, &envs)?;
                let TimedBuild {
                    elapsed_ms: elapsed,
                    restarted,
                    packages,
                    phases: pio_phases,
                } = timed_build(
                    kind,
                    board,
                    options,
                    repo_root,
                    project_dir,
                    fbuild,
                    arduino_build_dir,
                    log,
                    &envs,
                )?;
                let elapsed = elapsed + install_ms;
                if let (Some(before), Some(after)) = (rx_before, network_rx_bytes()) {
                    let received = after.saturating_sub(before);
                    check_cold_download(options.network_mbit, received, kind, board)?;
                    cold_download_trials_bytes.push(received);
                }
                daemon_restarts += usize::from(restarted);
                if matches!(kind, ToolKind::PlatformIo | ToolKind::Fbuild) {
                    record_package_metadata(
                        &mut resolved_packages,
                        &mut package_metadata_warning,
                        packages,
                        kind,
                        board,
                    );
                }
                cold_trials_ms.push(round_millis(elapsed));
                if let Some(phases) = pio_phases {
                    cold_phase_trials.push(phases);
                }
                if matches!(kind, ToolKind::Fbuild) {
                    let content = fs::read_to_string(&perf_log).unwrap_or_default();
                    if let Some(phases) = perf_phases_after(&content, offset, PERF_PHASE_LABELS) {
                        cold_phase_trials.push(phases);
                    }
                    if trial == 1 {
                        if let Some(ms) = measure_raw_baseline(
                            project_dir,
                            board.environment,
                            options.trials,
                            log,
                        )? {
                            raw_baselines_ms.insert(board.key.to_string(), ms);
                        }
                    }
                }
            }
            MeasurementStep::WarmBuild(_) => {
                let TimedBuild {
                    elapsed_ms: elapsed,
                    restarted,
                    ..
                } = timed_build(
                    kind,
                    board,
                    options,
                    repo_root,
                    project_dir,
                    fbuild,
                    arduino_build_dir,
                    log,
                    &envs,
                )?;
                daemon_restarts += usize::from(restarted);
                warm_trials_ms.push(round_millis(elapsed));
            }
        }
    }

    if matches!(kind, ToolKind::PlatformIo | ToolKind::Fbuild)
        && !package_metadata_is_complete(board.key, &resolved_packages)
    {
        let warning = package_metadata_warning.get_or_insert_with(|| {
            format!(
                "{} build output omitted one or more required resolved package identities",
                kind.style().label
            )
        });
        println!(
            "::warning title=benchmark package metadata unavailable::{} ({})",
            board.name, warning
        );
        writeln!(log, "Package metadata warning: {warning}")?;
    }
    if matches!(kind, ToolKind::PlatformIo) {
        run_logged_env(
            options.platformio.as_os_str(),
            &os_args(&[
                "run",
                "--project-dir",
                &project_dir.to_string_lossy(),
                "--environment",
                board.environment,
                "--target",
                "compiledb",
            ]),
            repo_root,
            log,
            &[],
        )?;
    }
    let compile_db = match kind {
        ToolKind::Arduino => None,
        ToolKind::PlatformIo => Some(NormalizedPath::new(
            project_dir.join("compile_commands.json"),
        )),
        ToolKind::Fbuild => find_compile_db(project_dir, board.environment),
    };
    let (core_source_count, core_compile_argv) = compile_db
        .as_deref()
        .filter(|path| path.is_file())
        .map(core_compile_metadata)
        .transpose()?
        .unwrap_or((None, None));

    let cold_ms = round_millis(median(&cold_trials_ms));
    let warm_ms = round_millis(median(&warm_trials_ms));
    let speedup = if warm_ms > 0.0 {
        round_to(cold_ms / warm_ms, 3)
    } else {
        0.0
    };
    let style = kind.style();
    Ok(ToolResult {
        board: board.key.to_string(),
        board_name: board.name.to_string(),
        tool: style.key.to_string(),
        display_name: style.label.to_string(),
        version: version.to_string(),
        cold_ms,
        warm_ms,
        speedup,
        cold_trials_ms,
        warm_trials_ms,
        cold_download_bytes: median_bytes(&cold_download_trials_bytes),
        cold_download_trials_bytes,
        cold_phases_ms: phase_medians(&cold_phase_trials),
        cold_phase_trials,
        daemon_restarts,
        resolved_packages,
        package_metadata_warning,
        core_source_count,
        core_compile_argv,
    })
}

/// Install what the build needs when the tool's build does not do it itself.
/// Only Arduino CLI needs this; it is part of that tool's cold cost.
fn timed_cold_install(
    kind: ToolKind,
    board: Board,
    options: &Options,
    repo_root: &Path,
    log: &mut File,
    envs: &[(&str, OsString)],
) -> AppResult<f64> {
    if !matches!(kind, ToolKind::Arduino) {
        return Ok(0.0);
    }
    // Multi-GB installs can hit transient HTTP/2 resets. Retries stay inside
    // the timer, so they can only make Arduino CLI's cold time look worse.
    let started = Instant::now();
    retry(COLD_INSTALL_ATTEMPTS, || {
        run_logged_env(
            &options.arduino_cli,
            &os_args(&["core", "install", board.arduino_core]),
            repo_root,
            log,
            envs,
        )
        .map(|_| ())
    })?;
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

fn retry(attempts: usize, mut action: impl FnMut() -> AppResult<()>) -> AppResult<()> {
    let mut remaining = attempts;
    loop {
        remaining -= 1;
        match action() {
            Err(_) if remaining > 0 => {}
            result => return result,
        }
    }
}

/// Bytes received on non-loopback interfaces, per Linux `/proc/net/dev`
/// content. Inside the benchmark namespace that is exactly the shaped link.
fn parse_net_dev_rx(content: &str) -> Option<u64> {
    let mut total = None;
    for line in content.lines().skip(2) {
        let (name, counters) = line.split_once(':')?;
        if name.trim() == "lo" {
            continue;
        }
        let received: u64 = counters.split_whitespace().next()?.parse().ok()?;
        total = Some(total.unwrap_or(0) + received);
    }
    total
}

fn network_rx_bytes() -> Option<u64> {
    parse_net_dev_rx(&fs::read_to_string("/proc/net/dev").ok()?)
}

/// Fail the run rather than publish a "cold" time that downloaded nothing.
fn check_cold_download(
    network_mbit: u32,
    received: u64,
    kind: ToolKind,
    board: Board,
) -> AppResult<()> {
    if network_mbit > 0 && received < MIN_COLD_DOWNLOAD_BYTES {
        return Err(io::Error::other(format!(
            "{} cold build on {} received only {received} bytes; cold cleanup left packages installed",
            kind.style().label,
            board.name
        ))
        .into());
    }
    Ok(())
}

fn median_bytes(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let samples = values.iter().map(|value| *value as f64).collect::<Vec<_>>();
    Some(median(&samples).round() as u64)
}

/// Human duration: seconds under a minute, otherwise minutes, two decimals.
fn format_duration(ms: f64) -> String {
    if ms >= 60_000.0 {
        format!("{:.2} min", ms / 60_000.0)
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

fn format_bytes(bytes: u64) -> String {
    let bytes = bytes as f64;
    if bytes >= 1e9 {
        format!("{:.2} GB", bytes / 1e9)
    } else {
        format!("{:.1} MB", bytes / 1e6)
    }
}

/// Arduino CLI's data directory, holding `packages/` and `staging/`.
fn arduino_data_dir(arduino_cli: &OsStr) -> AppResult<NormalizedPath> {
    let output = Command::new(arduino_cli)
        .args(["config", "get", "directories.data"])
        .output()?;
    let dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || dir.is_empty() {
        return Err(io::Error::other("arduino-cli did not report directories.data").into());
    }
    Ok(NormalizedPath::new(Path::new(&dir)))
}

/// Whether a command's stderr carries the CLI's daemon restart notice.
fn restarted_daemon(stderr: &[u8]) -> bool {
    String::from_utf8_lossy(stderr).contains(DAEMON_RESTART_NOTICE)
}

/// Append the tail of the fbuild daemon's own log, which records each
/// daemon's identity (image, mtime, launcher) at startup.
fn append_daemon_log_tail(log: &mut File) -> io::Result<()> {
    let daemon_log = fbuild_paths::get_daemon_log_file();
    writeln!(
        log,
        "\n===== fbuild daemon.log tail ({}) =====",
        daemon_log.display()
    )?;
    match fs::read_to_string(&daemon_log) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(DAEMON_LOG_TAIL_LINES);
            for line in &lines[start..] {
                writeln!(log, "{line}")?;
            }
        }
        Err(error) => writeln!(log, "(unreadable: {error})")?,
    }
    log.flush()
}

/// Number of lines currently in the perf log; a missing file counts as zero.
fn perf_line_count(path: &Path) -> usize {
    fs::read_to_string(path)
        .map(|content| content.lines().count())
        .unwrap_or(0)
}

/// Parse perf JSONL records appended after the first `offset` lines.
fn parse_perf_lines(content: &str, offset: usize) -> Vec<Value> {
    content
        .lines()
        .skip(offset)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Merged phases of the last record for each of `labels` appended after
/// `offset` lines. The AVR build emits two timers per build: the
/// `avr-orchestrator` one (toolchain/framework/scan) and the inner
/// `pipeline` one (compile/link), so both must be combined.
fn perf_phases_after(
    content: &str,
    offset: usize,
    labels: &[&str],
) -> Option<BTreeMap<String, f64>> {
    let records = parse_perf_lines(content, offset);
    let mut merged: Option<BTreeMap<String, f64>> = None;
    for label in labels {
        let Some(record) = records
            .iter()
            .rev()
            .find(|record| record["label"] == *label)
        else {
            continue;
        };
        let out = merged.get_or_insert_with(BTreeMap::new);
        if let Some(phases) = record["phases"].as_object() {
            for (name, ms) in phases {
                if let Some(ms) = ms.as_f64() {
                    *out.entry(name.clone()).or_insert(0.0) += ms;
                }
            }
        }
    }
    merged
}

/// Per-phase medians, each computed only over the trials that recorded that phase.
fn phase_medians(trials: &[BTreeMap<String, f64>]) -> BTreeMap<String, f64> {
    let mut samples: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for trial in trials {
        for (name, ms) in trial {
            samples.entry(name.clone()).or_default().push(*ms);
        }
    }
    samples
        .into_iter()
        .map(|(name, values)| (name, round_millis(median(&values))))
        .collect()
}

/// Locate the compile DB fbuild wrote for env `uno`, release profile.
///
/// Prefers `compile_commands.raw.json`, the real toolchain invocations
/// (FastLED/fbuild#1467). `compile_commands.json` is rewritten for clangd
/// (`clang++ --target=avr`, no `-flto`) and cannot be replayed, so it is
/// only a fallback for older fbuild builds.
fn find_compile_db(project_dir: &Path, environment: &str) -> Option<NormalizedPath> {
    fn search(dir: &Path, name: &str) -> Option<NormalizedPath> {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(NormalizedPath::from(candidate));
        }
        let mut subdirs = fs::read_dir(dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        subdirs.sort();
        subdirs.iter().find_map(|sub| search(sub, name))
    }
    // The timed builds use `--release`; `uno/quick` may also exist and must
    // not win the sorted search.
    let env_root = fbuild_paths::get_project_build_root(project_dir)
        .join(environment)
        .join("release");
    ["compile_commands.raw.json", "compile_commands.json"]
        .into_iter()
        .find_map(|name| search(&env_root, name))
        .or_else(|| {
            let fallback = project_dir.join("compile_commands.json");
            fallback.is_file().then(|| NormalizedPath::from(fallback))
        })
}

fn measure_raw_baseline(
    project_dir: &Path,
    environment: &str,
    trials: usize,
    log: &mut File,
) -> AppResult<Option<f64>> {
    let Some(db) = find_compile_db(project_dir, environment) else {
        eprintln!(
            "warning: no compile_commands.json found for {environment}; raw_baseline_ms = null"
        );
        writeln!(
            log,
            "warning: no compile_commands.json found; raw baseline skipped"
        )?;
        return Ok(None);
    };
    let entries: Vec<CompileEntry> = match fs::read_to_string(&db)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!(
                "warning: unreadable {}: {error}; raw_baseline_ms = null",
                db.display()
            );
            return Ok(None);
        }
    };
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    writeln!(
        log,
        "----- raw compiler baseline: {} entries from {} ({jobs} jobs) -----",
        entries.len(),
        db.display()
    )?;
    let mut samples = Vec::with_capacity(trials);
    for _ in 0..trials {
        match raw_baseline_ms(&entries, jobs) {
            Ok(ms) => samples.push(ms),
            Err(error) => {
                eprintln!("warning: raw compiler baseline failed: {error}; raw_baseline_ms = null");
                writeln!(log, "warning: raw compiler baseline failed: {error}")?;
                return Ok(None);
            }
        }
    }
    Ok(Some(round_millis(median(&samples))))
}

/// Split a compile DB `command` string, honouring double/single quotes and backslash escapes.
fn split_command(command: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if chars.peek().is_some_and(|next| *next == '"') {
                    let next = chars.next().expect("peeked character");
                    current.push(next);
                } else {
                    current.push('\\');
                }
            }
            (Some(_), c) => current.push(c),
            (None, '\\') => {
                if chars
                    .peek()
                    .is_some_and(|next| *next == '"' || next.is_whitespace())
                {
                    let next = chars.next().expect("peeked character");
                    current.push(next);
                    in_token = true;
                } else {
                    current.push('\\');
                    in_token = true;
                }
            }
            (None, '"' | '\'') => {
                quote = Some(c);
                in_token = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_token {
                    args.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_token = true;
            }
        }
    }
    if in_token {
        args.push(current);
    }
    args
}

fn entry_argv(entry: &CompileEntry) -> Vec<String> {
    match (&entry.arguments, &entry.command) {
        (Some(arguments), _) if !arguments.is_empty() => arguments.clone(),
        (_, Some(command)) => split_command(command),
        _ => Vec::new(),
    }
}

/// Drop leading compiler-wrapper argv entries (`zccache`, `fbuild`) and
/// redirect the object output to `output`.
fn rewrite_compile_argv(argv: &[String], output: &Path) -> Vec<String> {
    let is_wrapper = |arg: &String| {
        Path::new(arg)
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(|stem| stem == "zccache" || stem == "fbuild")
    };
    let start = argv.iter().take_while(|arg| is_wrapper(arg)).count();
    let output = output.to_string_lossy().into_owned();
    let mut rewritten = Vec::with_capacity(argv.len() - start + 2);
    let mut saw_output = false;
    let mut args = argv[start..].iter();
    while let Some(arg) = args.next() {
        if arg == "-o" {
            args.next();
            rewritten.push("-o".to_string());
            rewritten.push(output.clone());
            saw_output = true;
        } else if arg.len() > 2 && arg.starts_with("-o") {
            rewritten.push(format!("-o{output}"));
            saw_output = true;
        } else if arg.starts_with("-D") {
            // fbuild's raw DB retains shell-escaped macro quotes (\") even
            // though the original compile launches via a shell. Replay uses
            // Command::args, which must receive the unescaped quote itself.
            rewritten.push(arg.replace("\\\"", "\""));
        } else {
            rewritten.push(arg.clone());
        }
    }
    if !saw_output {
        rewritten.push("-o".to_string());
        rewritten.push(output);
    }
    rewritten
}

/// Replay every compile DB entry with the bare compiler across `jobs` threads.
fn raw_baseline_ms(entries: &[CompileEntry], jobs: usize) -> AppResult<f64> {
    let temp = tempfile::TempDir::new_in(fbuild_paths::temp_subdir("fastled-examples-bench"))?;
    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<String>> = Mutex::new(None);
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(entry) = entries.get(index) else {
                        break;
                    };
                    let output = temp.path().join(format!("{index}.o"));
                    let argv = rewrite_compile_argv(&entry_argv(entry), &output);
                    let Some((program, args)) = argv.split_first() else {
                        continue;
                    };
                    let result = Command::new(program)
                        .args(args)
                        .current_dir(&entry.directory)
                        .output();
                    let error = match result {
                        Ok(out) if out.status.success() => None,
                        Ok(out) => Some(format!(
                            "{program} exited with {}: {}",
                            out.status,
                            String::from_utf8_lossy(&out.stderr).trim()
                        )),
                        Err(error) => Some(format!("{program}: {error}")),
                    };
                    if let Some(error) = error {
                        let mut slot = failure.lock().unwrap_or_else(|p| p.into_inner());
                        if slot.is_none() {
                            *slot = Some(error);
                        }
                        next.store(entries.len(), Ordering::Relaxed);
                        break;
                    }
                }
            });
        }
    });
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    if let Some(error) = failure.into_inner().unwrap_or_else(|p| p.into_inner()) {
        return Err(io::Error::other(error).into());
    }
    Ok(round_millis(elapsed))
}

fn measurement_plan(trials: usize) -> Vec<MeasurementStep> {
    (1..=trials)
        .flat_map(|trial| {
            [
                MeasurementStep::PrepareCold(trial),
                MeasurementStep::ColdBuild(trial),
                MeasurementStep::WarmBuild(trial),
            ]
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn prepare_cold(
    kind: ToolKind,
    board: Board,
    options: &Options,
    repo_root: &Path,
    project_dir: &Path,
    fbuild: &Path,
    arduino_build_dir: &Path,
    envs: &[(&str, OsString)],
    log: &mut File,
) -> AppResult<()> {
    writeln!(log, "----- untimed cold-cache preparation -----")?;
    for step in cold_cleanup_steps(
        kind,
        board,
        &options.arduino_cli,
        &options.platformio,
        project_dir,
        fbuild,
        arduino_build_dir,
        &fbuild_paths::get_cache_root(),
        &fbuild_paths::get_platformio_home(),
        &arduino_data_dir(&options.arduino_cli)?,
    ) {
        match step {
            ColdCleanupStep::Command { program, args } => {
                run_logged_env(&program, &args, repo_root, log, envs)?;
            }
            ColdCleanupStep::RemoveDir(path) => remove_dir_within(repo_root, &path)?,
            ColdCleanupStep::RemoveCacheEntries { parent, prefix } => {
                remove_cache_entries(&parent, &prefix)?;
            }
        }
    }
    writeln!(log, "----- timed cold build follows -----")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cold_cleanup_steps(
    kind: ToolKind,
    board: Board,
    arduino_cli: &OsStr,
    platformio: &OsStr,
    project_dir: &Path,
    fbuild: &Path,
    arduino_build_dir: &Path,
    fbuild_cache_root: &Path,
    platformio_home: &Path,
    arduino_data: &Path,
) -> Vec<ColdCleanupStep> {
    let command = |program: &OsStr, args: Vec<OsString>| ColdCleanupStep::Command {
        program: program.to_os_string(),
        args,
    };
    let entries = |parent: &Path, prefix: &str| ColdCleanupStep::RemoveCacheEntries {
        parent: NormalizedPath::new(parent),
        prefix: prefix.to_string(),
    };
    // Cold means a first-time install: every tool loses all packages and
    // toolchains, on every board, so the download lands in the timed build.
    match kind {
        ToolKind::Arduino => vec![
            command(arduino_cli, os_args(&["cache", "clean"])),
            ColdCleanupStep::RemoveDir(NormalizedPath::new(arduino_build_dir)),
            entries(arduino_data, "packages"),
            entries(arduino_data, "staging"),
        ],
        ToolKind::PlatformIo => vec![
            command(
                platformio,
                os_args(&["system", "prune", "--cache", "--force"]),
            ),
            ColdCleanupStep::RemoveDir(NormalizedPath::new(project_dir.join(".pio"))),
            // The whole home: pioarduino also reuses downloads from `dist` and
            // `tools`, which made later trials fetch ~230 MB less than the first.
            entries(platformio_home, ""),
        ],
        ToolKind::Fbuild => vec![
            command(
                fbuild.as_os_str(),
                os_args(&[
                    "clean",
                    "cache",
                    &project_dir.to_string_lossy(),
                    "--environment",
                    board.environment,
                    "--release",
                ]),
            ),
            // The daemon holds the cache database open; the timed build starts
            // a new one, which a first-time user pays for too.
            command(fbuild.as_os_str(), os_args(&["daemon", "stop"])),
            entries(fbuild_cache_root, ""),
            ColdCleanupStep::RemoveDir(NormalizedPath::new(get_project_fbuild_dir(project_dir))),
        ],
    }
}

#[allow(clippy::too_many_arguments)]
fn timed_build(
    kind: ToolKind,
    board: Board,
    options: &Options,
    repo_root: &Path,
    project_dir: &Path,
    fbuild: &Path,
    arduino_build_dir: &Path,
    log: &mut File,
    envs: &[(&str, OsString)],
) -> AppResult<TimedBuild> {
    let (program, args) = match kind {
        ToolKind::Arduino => (
            options.arduino_cli.as_os_str(),
            os_args(&[
                "compile",
                "--fqbn",
                board.fqbn,
                "--build-path",
                &arduino_build_dir.to_string_lossy(),
                &project_dir.to_string_lossy(),
            ]),
        ),
        ToolKind::PlatformIo => (
            options.platformio.as_os_str(),
            platformio_build_args(board, project_dir),
        ),
        ToolKind::Fbuild => (
            fbuild.as_os_str(),
            os_args(&[
                "build",
                &project_dir.to_string_lossy(),
                "--environment",
                board.environment,
                "--release",
            ]),
        ),
    };

    let started = Instant::now();
    let (output, lines) = run_logged_env_stamped(program, &args, repo_root, log, envs)?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let packages = match kind {
        ToolKind::PlatformIo => parse_platformio_packages(&output.stdout, board),
        ToolKind::Fbuild => parse_fbuild_build_packages(&output.stdout, board),
        ToolKind::Arduino => BTreeMap::new(),
    };
    let phases = match kind {
        ToolKind::PlatformIo => pio_phases::phases(&lines, elapsed_ms),
        _ => None,
    };
    Ok(TimedBuild {
        elapsed_ms,
        restarted: restarted_daemon(&output.stderr),
        packages,
        phases,
    })
}

/// `-v` prints each command as it starts, the phase breakdown's input. pioarduino
/// crashes SCons under `-v` (`'_Null' + str`), so ESP32-S3 has no PlatformIO phases.
fn platformio_build_args(board: Board, project_dir: &Path) -> Vec<OsString> {
    let project = project_dir.to_string_lossy();
    let mut args = vec![
        "run",
        "--project-dir",
        &project,
        "--environment",
        board.environment,
    ];
    if board.key != "esp32s3" {
        args.push("-v");
    }
    os_args(&args)
}

struct TimedBuild {
    elapsed_ms: f64,
    restarted: bool,
    packages: BTreeMap<String, String>,
    /// PlatformIO phase breakdown parsed from `-v` output (see `pio_phases`).
    phases: Option<BTreeMap<String, f64>>,
}

/// [`run_logged_env`] that also stamps every stdout line with its arrival
/// time (ms since spawn). `PYTHONUNBUFFERED` keeps SCons from batching
/// output, so a stamp is when the command started, not when a buffer filled.
fn run_logged_env_stamped(
    program: &OsStr,
    args: &[OsString],
    cwd: &Path,
    log: &mut File,
    envs: &[(&str, OsString)],
) -> AppResult<(Output, Vec<(f64, String)>)> {
    use std::io::{BufRead, Read};
    writeln!(log, "$ {}", display_command(program, args))?;
    log.flush()?;
    let started = Instant::now();
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("CI", "true")
        .env("PLATFORMIO_SETTING_ENABLE_TELEMETRY", "no")
        .env("PYTHONUNBUFFERED", "1")
        .envs(envs.iter().map(|(key, value)| (*key, value)))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut bytes);
        bytes
    });
    let mut stdout = Vec::new();
    let mut lines = Vec::new();
    let mut reader = io::BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line)? > 0 {
        let at = started.elapsed().as_secs_f64() * 1000.0;
        stdout.extend_from_slice(&line);
        lines.push((at, String::from_utf8_lossy(&line).trim_end().to_string()));
        line.clear();
    }
    let status = child.wait()?;
    let stderr = stderr_reader.join().unwrap_or_default();
    log.write_all(&stdout)?;
    log.write_all(&stderr)?;
    log.flush()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "command failed with {status}: {}; see benchmark-output/benchmark.log",
            display_command(program, args)
        ))
        .into());
    }
    Ok((
        Output {
            status,
            stdout,
            stderr,
        },
        lines,
    ))
}

fn record_package_metadata(
    current: &mut BTreeMap<String, String>,
    warning: &mut Option<String>,
    observed: BTreeMap<String, String>,
    kind: ToolKind,
    board: Board,
) {
    if observed.is_empty() {
        current.clear();
        if warning.is_none() {
            *warning = Some(format!(
                "{} build output omitted resolved package identities on {}",
                kind.style().label,
                board.name
            ));
        }
        return;
    }
    // Every timed cold trial must agree. Once any trial is missing or has a
    // different identity, later observations cannot restore comparability.
    if warning.is_some() {
        return;
    }
    if current.is_empty() {
        *current = observed;
    } else if *current != observed {
        current.clear();
        *warning = Some(format!(
            "{} resolved package identities changed across cold trials on {}",
            kind.style().label,
            board.name
        ));
    }
}

fn parse_fbuild_build_packages(stdout: &[u8], board: Board) -> BTreeMap<String, String> {
    let mut packages = BTreeMap::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        if board.key == "esp32s3" {
            let Some((_, resolved)) = line
                .split_once("ESP32 packages:")
                .and_then(|(_, packages)| packages.split_once("; resolved "))
            else {
                continue;
            };
            for (key, marker) in [
                ("platform", "platform="),
                ("framework", "framework="),
                ("toolchain", "toolchain="),
                ("sdk", "ESP-IDF SDK="),
            ] {
                if let Some(value) = resolved
                    .split_once(marker)
                    .map(|(_, value)| value.split([',', ';']).next().unwrap_or("").trim())
                {
                    let version = value.rsplit_once('@').map_or(value, |(_, version)| version);
                    if !version.is_empty() {
                        packages.insert(key.to_string(), normalize_toolchain_version(version));
                    }
                }
            }
        } else if board.key == "uno" {
            if let Some((_, resolved)) = line.split_once("AVR resolved:") {
                // Entries look like `name@version (url); name@version (url)`,
                // with fbuild's own names (`avr-gcc`, `arduino-avr-core`).
                for entry in resolved.split(';') {
                    let Some((name, rest)) = entry.trim().split_once('@') else {
                        continue;
                    };
                    let version = rest.split_whitespace().next().unwrap_or("");
                    let key = if name == "avr-gcc" || name.starts_with("toolchain-atmelavr") {
                        "toolchain"
                    } else if name == "arduino-avr-core" || name.starts_with("framework-") {
                        "framework"
                    } else {
                        continue;
                    };
                    if !version.is_empty() {
                        packages.insert(key.to_string(), version.trim_end_matches(',').to_string());
                    }
                }
            }
        }
    }
    packages
}

/// fbuild names a toolchain by its archive stem
/// (`xtensa-esp-elf-14.2.0_20260121-x86_64-linux-gnu`) where PlatformIO reports
/// the same release as `14.2.0+20260121`.
fn normalize_toolchain_version(version: &str) -> String {
    version
        .strip_prefix("xtensa-esp-elf-")
        .and_then(|rest| rest.split('-').next())
        .map_or_else(|| version.to_string(), |rest| rest.replace('_', "+"))
}

/// Version from a platform pin: `name@1.2.3`, or a `.../download/<tag>/x.zip` URL.
fn platform_pin_version(pin: &str) -> &str {
    if let Some((_, rest)) = pin.split_once("/download/") {
        return rest.split('/').next().unwrap_or(pin);
    }
    pin.rsplit_once('@').map_or(pin, |(_, version)| version)
}

fn parse_platformio_packages(stdout: &[u8], board: Board) -> BTreeMap<String, String> {
    let mut packages = BTreeMap::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        if let Some(spec) = line.split("platform: ").nth(1) {
            if let Some(pin) = spec.split(';').next() {
                packages.insert(
                    "platform".to_string(),
                    platform_pin_version(pin.trim()).to_string(),
                );
            }
        }
        let Some((name, version)) = line
            .trim()
            .strip_prefix("- ")
            .and_then(|line| line.split_once(" @ "))
        else {
            continue;
        };
        let key = if name.starts_with("framework-") && !name.ends_with("-libs") {
            Some("framework")
        } else if name.starts_with("toolchain-")
            && (board.key != "esp32s3" || name.starts_with("toolchain-xtensa-esp"))
        {
            Some("toolchain")
        } else if name == "tool-esptoolpy" {
            Some("flash_tool")
        } else {
            None
        };
        if let Some(key) = key {
            packages.insert(
                key.to_string(),
                version.split_whitespace().next().unwrap_or("").to_string(),
            );
        }
    }
    packages
}

fn core_compile_metadata(path: &Path) -> AppResult<(Option<usize>, Option<Vec<String>>)> {
    let entries: Vec<CompileEntry> = serde_json::from_slice(&fs::read(path)?)?;
    let core = entries
        .iter()
        .filter(|entry| {
            NormalizedPath::new(Path::new(&entry.file))
                .display_slash()
                .contains("/cores/esp32/")
        })
        .collect::<Vec<_>>();
    if core.is_empty() {
        return Ok((None, None));
    }
    let source_count = core
        .iter()
        .map(|entry| &entry.file)
        .collect::<BTreeSet<_>>()
        .len();
    let representative = core
        .iter()
        .find(|entry| entry.file.ends_with("Esp.cpp"))
        .copied()
        .unwrap_or(core[0]);
    Ok((Some(source_count), Some(entry_argv(representative))))
}

fn os_args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn run_logged_env(
    program: &OsStr,
    args: &[OsString],
    cwd: &Path,
    log: &mut File,
    envs: &[(&str, OsString)],
) -> AppResult<Output> {
    writeln!(log, "$ {}", display_command(program, args))?;
    log.flush()?;
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("CI", "true")
        .env("PLATFORMIO_SETTING_ENABLE_TELEMETRY", "no")
        .envs(envs.iter().map(|(key, value)| (*key, value)))
        .output()?;
    log.write_all(&output.stdout)?;
    log.write_all(&output.stderr)?;
    log.flush()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "command failed with {}: {}; see benchmark-output/benchmark.log",
            output.status,
            display_command(program, args)
        ))
        .into());
    }
    Ok(output)
}

fn display_command(program: &OsStr, args: &[OsString]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(OsString::as_os_str))
        .map(|part| format!("{:?}", part.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn command_version(program: &OsStr, args: &[&str]) -> AppResult<String> {
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!("failed to query version from {:?}", program)).into());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let version = if stdout.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    Ok(version.lines().next().unwrap_or("unknown").to_string())
}

fn remove_dir_within(root: &Path, target: &Path) -> AppResult<()> {
    if !target.starts_with(root) || target == root {
        return Err(io::Error::other(format!(
            "refusing to remove benchmark path outside repository: {}",
            target.display()
        ))
        .into());
    }
    match fs::remove_dir_all(target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Delete immediate children of `parent` whose names start with `prefix`.
/// Missing parents are a no-op so a first run on a clean machine works.
fn remove_cache_entries(parent: &Path, prefix: &str) -> AppResult<()> {
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // Substring, not prefix: fbuild also stores the same package under an
        // owner-prefixed name (`espressif-framework-arduinoespressif32`) and a
        // download-cache name (`dl-framework-arduinoespressif32-<ver>-<sha>`).
        // A prefix match missed those, so the ESP32 framework survived the cold
        // cleanup and fbuild "cold" builds skipped the download entirely.
        if !name.contains(prefix) {
            continue;
        }
        let path = entry.path();
        // symlink_metadata: never traverse a symlink out of the cache root.
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn absolute_from(root: &Path, path: &Path) -> NormalizedPath {
    if path.is_absolute() {
        NormalizedPath::new(path)
    } else {
        NormalizedPath::new(root.join(path))
    }
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn round_millis(value: f64) -> f64 {
    round_to(value, 3)
}

fn round_to(value: f64, places: i32) -> f64 {
    let factor = 10_f64.powi(places);
    (value * factor).round() / factor
}

fn write_outputs(
    output_dir: &Path,
    metadata: &Metadata,
    results: &[ToolResult],
    pages_url: &str,
    raw_base_url: &str,
) -> AppResult<()> {
    fs::create_dir_all(output_dir)?;
    let latest = latest_payload(metadata, results);
    write_json(NormalizedPath::new(output_dir.join("latest.json")), &latest)?;
    write_history(
        NormalizedPath::new(output_dir.join("history.jsonl")),
        metadata,
        results,
    )?;
    write_json(
        NormalizedPath::new(output_dir.join("manifest.json")),
        &manifest_payload(metadata, pages_url, raw_base_url),
    )?;
    fs::write(
        output_dir.join("benchmark.svg"),
        render_svg(metadata, results),
    )?;
    fs::write(
        output_dir.join("index.html"),
        render_html(metadata, results),
    )?;
    fs::write(output_dir.join(".nojekyll"), "")?;
    Ok(())
}

fn latest_payload(metadata: &Metadata, results: &[ToolResult]) -> Value {
    json!({
        "schema_version": 2,
        "metadata": {
            "generated_at": metadata.generated_at,
            "git_sha": metadata.git_sha,
            "repository": metadata.repository,
            "run_url": metadata.run_url,
            "runner": {
                "os": env::consts::OS,
                "arch": env::consts::ARCH,
            },
            "fixture": metadata.project,
            "boards": BOARDS.iter().map(|board| json!({
                "key": board.key,
                "name": board.name,
                "environment": board.environment,
                "fqbn": board.fqbn,
            })).collect::<Vec<_>>(),
            "toolchain_pins": {
                "uno": {
                    "arduino_core": "arduino:avr@1.8.8",
                    "platformio_platform": "atmelavr@5.1.0",
                },
                "esp32s3": {
                    "arduino_core": "esp32:esp32@3.3.12",
                    "platformio_platform": "pioarduino espressif32 55.03.312",
                },
                "note": "all three tools build ESP32-S3 against Arduino-ESP32 3.3.12",
            },
            "trials": metadata.trials,
            "network_mbit": metadata.network_mbit,
            "statistic": "median",
            "cold_definition": "first-time install: project outputs, caches, packages and toolchains removed for every tool and board, so all downloads land inside the timed window (Arduino CLI's includes core install)",
            "warm_definition": "immediate no-change rebuild after the cold build",
        },
        "comparison_board": comparison_board(results),
        "raw_baseline_ms": selected_raw_baseline_ms(metadata, results),
        "raw_baselines_ms": metadata.raw_baselines_ms,
        "fbuild_overhead_ms": fbuild_overhead_ms(metadata, results),
        "fbuild_vs_platformio_cold": fbuild_vs_platformio_cold(results),
        "board_metrics": board_metrics(metadata, results),
        "results": results,
    })
}

fn cold_of(results: &[ToolResult], board: &str, tool: &str) -> Option<f64> {
    results
        .iter()
        .find(|result| result.board == board && result.tool == tool)
        .map(|result| result.cold_ms)
}

fn fbuild_overhead_ms(metadata: &Metadata, results: &[ToolResult]) -> Option<f64> {
    let board = comparison_board(results)?;
    Some(round_millis(
        cold_of(results, board, "fbuild")? - selected_raw_baseline_ms(metadata, results)?,
    ))
}

fn selected_raw_baseline_ms(metadata: &Metadata, results: &[ToolResult]) -> Option<f64> {
    metadata
        .raw_baselines_ms
        .get(comparison_board(results)?)
        .copied()
}

fn board_cold_ratio(results: &[ToolResult], board: &str) -> Option<f64> {
    let fbuild = results
        .iter()
        .find(|result| result.board == board && result.tool == "fbuild")?;
    let platformio = results
        .iter()
        .find(|result| result.board == board && result.tool == "platformio")?;
    if !board_stack_comparable(results, board) || platformio.cold_ms <= 0.0 {
        return None;
    }
    Some(round_to(fbuild.cold_ms / platformio.cold_ms, 3))
}

fn board_stack_comparable(results: &[ToolResult], board: &str) -> bool {
    board_stack_status(results, board) == StackStatus::Matched
}

fn stack_package_keys(board: &str) -> &'static [&'static str] {
    match board {
        "esp32s3" => &["platform", "framework", "toolchain"],
        "uno" => &["framework", "toolchain"],
        _ => &[],
    }
}

fn package_metadata_is_complete(board: &str, packages: &BTreeMap<String, String>) -> bool {
    let required = stack_package_keys(board);
    !required.is_empty() && required.iter().all(|key| packages.contains_key(*key))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StackStatus {
    Matched,
    Different,
    Unverified,
}

fn board_stack_status(results: &[ToolResult], board: &str) -> StackStatus {
    results
        .iter()
        .find(|result| result.board == board && result.tool == "fbuild")
        .zip(
            results
                .iter()
                .find(|result| result.board == board && result.tool == "platformio"),
        )
        .map_or(StackStatus::Unverified, |(fbuild, pio)| {
            let keys = stack_package_keys(board);
            if keys.is_empty() {
                return StackStatus::Unverified;
            }
            let resolved = keys
                .iter()
                .map(|key| {
                    fbuild
                        .resolved_packages
                        .get(*key)
                        .zip(pio.resolved_packages.get(*key))
                })
                .collect::<Vec<_>>();
            if resolved.iter().any(Option::is_none) {
                StackStatus::Unverified
            } else if resolved
                .iter()
                .all(|pair| pair.is_some_and(|(fbuild, pio)| fbuild == pio))
            {
                StackStatus::Matched
            } else {
                StackStatus::Different
            }
        })
}

fn board_metrics(metadata: &Metadata, results: &[ToolResult]) -> Value {
    let entries = BOARDS
        .iter()
        .map(|board| {
            (
                board.key.to_string(),
                json!({
                    "fbuild_vs_platformio_cold": board_cold_ratio(results, board.key),
                    "stack_comparable": board_stack_comparable(results, board.key),
                    "stack_status": match board_stack_status(results, board.key) {
                        StackStatus::Matched => "matched",
                        StackStatus::Different => "different",
                        StackStatus::Unverified => "unverified",
                    },
                    "raw_baseline_ms": metadata.raw_baselines_ms.get(board.key),
                    "fbuild_overhead_ms": cold_of(results, board.key, "fbuild")
                        .zip(metadata.raw_baselines_ms.get(board.key))
                        .map(|(cold, raw)| round_millis(cold - raw)),
                }),
            )
        })
        .collect::<serde_json::Map<String, Value>>();
    Value::Object(entries)
}

fn fbuild_vs_platformio_cold(results: &[ToolResult]) -> Option<f64> {
    board_cold_ratio(results, comparison_board(results)?)
}

fn comparison_board(results: &[ToolResult]) -> Option<&'static str> {
    BOARDS
        .iter()
        .filter_map(|board| board_cold_ratio(results, board.key).map(|ratio| (board.key, ratio)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(board, _)| board)
}

fn read_history_values(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse a history `ts`: either `unix:<seconds>` or ISO8601 `YYYY-MM-DDTHH:MM:SS...`.
fn parse_timestamp_unix_s(ts: &str) -> Option<u64> {
    if let Some(seconds) = ts.strip_prefix("unix:") {
        return seconds.parse().ok();
    }
    let field = |range: std::ops::Range<usize>| ts.get(range)?.parse::<i64>().ok();
    let days = days_from_civil(field(0..4)?, field(5..7)?, field(8..10)?);
    let seconds = days * 86_400 + field(11..13)? * 3_600 + field(14..16)? * 60 + field(17..19)?;
    u64::try_from(seconds).ok()
}

/// 7-day median of `fbuild_vs_platformio_cold`, returned only when `current_ratio` exceeds it.
fn ratio_regressed(history: &[Value], now_unix_s: u64, current_ratio: f64) -> Option<f64> {
    let recent = history
        .iter()
        .filter(|entry| {
            entry["ts"]
                .as_str()
                .and_then(parse_timestamp_unix_s)
                .is_some_and(|ts| ts <= now_unix_s && now_unix_s - ts <= REGRESSION_WINDOW_S)
        })
        .filter_map(|entry| entry["fbuild_vs_platformio_cold"].as_f64())
        .collect::<Vec<_>>();
    if recent.is_empty() {
        return None;
    }
    let baseline = median(&recent);
    (current_ratio > baseline).then_some(baseline)
}

fn ratio_regressed_for_board(
    history: &[Value],
    now_unix_s: u64,
    current_ratio: f64,
    board: &str,
) -> Option<f64> {
    let matching = history
        .iter()
        .filter_map(|entry| {
            let ratio = if entry.get("board_metrics").is_some() {
                entry["board_metrics"][board]["fbuild_vs_platformio_cold"].as_f64()
            } else if board == "uno" {
                entry["fbuild_vs_platformio_cold"].as_f64()
            } else {
                None
            }?;
            Some(json!({"ts": entry["ts"], "fbuild_vs_platformio_cold": ratio}))
        })
        .collect::<Vec<_>>();
    ratio_regressed(&matching, now_unix_s, current_ratio)
}

fn manifest_payload(metadata: &Metadata, pages_url: &str, raw_base_url: &str) -> Value {
    let raw = raw_base_url.trim_end_matches('/');
    json!({
        "schema_version": 1,
        "generated_at": metadata.generated_at,
        "git_sha": metadata.git_sha,
        "branch": "benchmark-stats",
        "repository": metadata.repository,
        "artifacts": {
            "manifest": {
                "description": "Discovery index regenerated by every benchmark run.",
                "url": format!("{raw}/manifest.json"),
                "content_type": "application/json",
                "schema_version": 1,
            },
            "latest": {
                "description": "Full metadata, raw trials, and median cold/warm timings for the newest run.",
                "url": format!("{raw}/latest.json"),
                "content_type": "application/json",
                "schema_version": 2,
            },
            "history": {
                "description": "Rolling compact history, one JSON object per benchmark run.",
                "url": format!("{raw}/history.jsonl"),
                "content_type": "application/x-ndjson",
                "schema_version": 1,
                "max_lines": HISTORY_MAX_LINES,
            },
            "image": {
                "description": "GitHub-dark charts: separate cold and warm panels per board.",
                "url": format!("{raw}/benchmark.svg"),
                "content_type": "image/svg+xml",
            },
            "site": {
                "description": "Human-facing benchmark report.",
                "url": pages_url,
                "content_type": "text/html",
            },
        },
    })
}

fn write_json(path: NormalizedPath, payload: &Value) -> AppResult<()> {
    fs::write(path, serde_json::to_string_pretty(payload)? + "\n")?;
    Ok(())
}

fn write_history(
    path: NormalizedPath,
    metadata: &Metadata,
    results: &[ToolResult],
) -> AppResult<()> {
    let mut prior = if path.is_file() {
        fs::read_to_string(&path)?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if prior.len() >= HISTORY_MAX_LINES {
        let discard = prior.len() - (HISTORY_MAX_LINES - 1);
        prior.drain(..discard);
    }
    let compact_results = results
        .iter()
        .map(|result| {
            json!({
                "board": result.board,
                "board_name": result.board_name,
                "tool": result.tool,
                "cold_ms": result.cold_ms,
                "warm_ms": result.warm_ms,
                "speedup": result.speedup,
            })
        })
        .collect::<Vec<_>>();
    prior.push(serde_json::to_string(&json!({
        "ts": metadata.generated_at,
        "sha": metadata.git_sha,
        "comparison_board": comparison_board(results),
        "fbuild_overhead_ms": fbuild_overhead_ms(metadata, results),
        "fbuild_vs_platformio_cold": fbuild_vs_platformio_cold(results),
        "board_metrics": board_metrics(metadata, results),
        "results": compact_results,
    }))?);
    fs::write(path, prior.join("\n") + "\n")?;
    Ok(())
}

/// Which median a chart panel plots.
#[derive(Clone, Copy)]
enum Phase {
    Cold,
    Warm,
}

impl Phase {
    fn key(self) -> &'static str {
        match self {
            Phase::Cold => "cold",
            Phase::Warm => "warm",
        }
    }

    fn ms(self, result: &ToolResult) -> f64 {
        match self {
            Phase::Cold => result.cold_ms,
            Phase::Warm => result.warm_ms,
        }
    }
}

const SVG_PANEL_TOP: f64 = 60.0;
const SVG_ROW_HEIGHT: f64 = 48.0;
const SVG_PANEL_GAP: f64 = 34.0;
/// Bar outline marking cold-build panels (ice blue).
const SVG_COLD_OUTLINE: &str = "#a5f3fc";
/// Bar outline marking warm-build panels (bright red).
const SVG_WARM_OUTLINE: &str = "#ff3b30";

fn tool_kind(tool: &str) -> ToolKind {
    match tool {
        "arduino" => ToolKind::Arduino,
        "platformio" => ToolKind::PlatformIo,
        _ => ToolKind::Fbuild,
    }
}

/// One board/phase chart panel scaled to its own slowest median; returns markup and height.
fn svg_panel(results: &[ToolResult], board: &str, phase: Phase, y0: f64) -> (String, f64) {
    let bar_x = 190.0;
    let bar_width = 480.0;
    let rows = results
        .iter()
        .filter(|result| result.board == board)
        .collect::<Vec<_>>();
    let board_name = rows
        .first()
        .map_or(board, |result| result.board_name.as_str());
    let max_ms = rows
        .iter()
        .map(|result| phase.ms(result))
        .fold(0.0_f64, f64::max)
        .max(0.001);
    let stack_note = if matches!(phase, Phase::Cold) {
        match board_stack_status(results, board) {
            StackStatus::Matched => " | fbuild/PIO stack matched",
            StackStatus::Different => " | fbuild/PIO stack differs; ratio excluded",
            StackStatus::Unverified => " | fbuild/PIO stack unverified; ratio excluded",
        }
    } else {
        ""
    };
    let (title, subtitle) = match phase {
        Phase::Cold => (
            "cold build (nothing installed)",
            "Scale: slowest cold median",
        ),
        Phase::Warm => (
            "warm build (no-change rebuild)",
            "Scale: slowest warm median",
        ),
    };
    let mut out = format!(
        r##"  <g role="group" aria-labelledby="{board}-{key}-title">
    <text id="{board}-{key}-title" x="24" y="{title_y:.1}" class="board">{name} — {title}</text>
    <text x="24" y="{scale_y:.1}" class="legend">{subtitle} = {max}{stack_note}</text>
"##,
        key = phase.key(),
        title_y = y0 + 22.0,
        name = xml_escape(board_name),
        scale_y = y0 + 44.0,
        max = format_duration(max_ms),
    );
    for (index, result) in rows.iter().enumerate() {
        let style = tool_kind(&result.tool).style();
        let y = y0 + SVG_PANEL_TOP + index as f64 * SVG_ROW_HEIGHT;
        let ms = phase.ms(result);
        let fill = style.warm_color;
        let outline = match phase {
            Phase::Cold => SVG_COLD_OUTLINE,
            Phase::Warm => SVG_WARM_OUTLINE,
        };
        let detail = match phase {
            Phase::Cold => result
                .cold_download_bytes
                .map_or_else(String::new, |bytes| {
                    format!("{} download", format_bytes(bytes))
                }),
            Phase::Warm => format!("{:.2}x faster than cold", result.speedup),
        };
        out.push_str(&format!(
            r##"    <text x="24" y="{label_y:.1}" class="tool" fill="{label_color}">{label}</text>
    <rect x="{bar_x:.1}" y="{y:.1}" width="{bar_width:.1}" height="28" rx="4" fill="#21262d" />
    <rect x="{bar_x:.1}" y="{y:.1}" width="{width:.1}" height="28" rx="4" fill="{fill}" stroke="{outline}" stroke-width="2">
      <title>{label} {key} median: {text}</title>
    </rect>
    <text x="692" y="{value_y:.1}" class="value">{key} {text}</text>
    <text x="692" y="{detail_y:.1}" class="speed">{detail}</text>
"##,
            label_y = y + 21.0,
            label_color = style.warm_color,
            label = xml_escape(&result.display_name),
            width = (ms / max_ms * bar_width).max(3.0),
            key = phase.key(),
            text = format_duration(ms),
            value_y = y + 13.0,
            detail_y = y + 29.0,
        ));
    }
    out.push_str("  </g>\n");
    let height = SVG_PANEL_TOP + rows.len() as f64 * SVG_ROW_HEIGHT + SVG_PANEL_GAP;
    (out, height)
}

fn render_svg(metadata: &Metadata, results: &[ToolResult]) -> String {
    let width = 1100.0;
    let mut y = 128.0;
    let mut panels = String::new();
    for board in BOARDS {
        for phase in [Phase::Cold, Phase::Warm] {
            let (markup, height) = svg_panel(results, board.key, phase, y);
            panels.push_str(&markup);
            y += height;
        }
    }
    let rule_y = y;
    let height = rule_y + 28.0;
    let short_sha = metadata.git_sha.chars().take(12).collect::<String>();
    let floor_line = svg_floor_line(metadata, results);
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width:.0}" height="{height:.0}" viewBox="0 0 {width:.0} {height:.0}" role="img" aria-labelledby="title description">
  <title id="title">Arduino Uno and ESP32-S3 Blink build benchmark</title>
  <desc id="description">Separate cold and warm build charts for Arduino Uno and ESP32-S3 Blink sketches, each scaled to its own slowest median.</desc>
  <style>
    text {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
    .heading {{ font-size: 30px; font-weight: 700; fill: #f0f6fc; }}
    .meta {{ font-size: 14px; fill: #8b949e; }}
    .legend {{ font-size: 14px; font-weight: 600; fill: #c9d1d9; }}
    .board {{ font-size: 20px; font-weight: 700; fill: #f0f6fc; }}
    .tool {{ font-size: 18px; font-weight: 700; }}
    .value {{ font-size: 14px; font-weight: 600; fill: #f0f6fc; }}
    .speed {{ font-size: 13px; fill: #8b949e; }}
  </style>
  <rect width="{width:.0}" height="{height:.0}" fill="#0d1117" />
  <rect width="{width:.0}" height="112" fill="#161b22" />
  <text x="24" y="43" class="heading">Arduino Uno + ESP32-S3 Blink build times</text>
  <text x="24" y="72" class="meta">Arduino CLI vs PlatformIO vs fbuild | median of {trials} trials{link}</text>
  <text x="24" y="95" class="meta">Generated {generated_at} | sha {sha}</text>
{floor_line}{panels}  <line x1="24" y1="{rule_y:.0}" x2="1076" y2="{rule_y:.0}" stroke="#30363d" stroke-width="2" />
  <text x="24" y="{foot_y:.0}" class="meta">Machine data: manifest.json | latest.json | history.jsonl</text>
</svg>
"##,
        trials = metadata.trials,
        link = if metadata.network_mbit > 0 {
            format!(
                " | {} Mbit/s link, nothing installed when cold",
                metadata.network_mbit
            )
        } else {
            String::new()
        },
        generated_at = xml_escape(&metadata.generated_at),
        sha = xml_escape(&short_sha),
        foot_y = rule_y + 20.0,
    )
}

/// Render the SVG metadata line, omitting unavailable floor and overhead values.
fn svg_floor_line(metadata: &Metadata, results: &[ToolResult]) -> String {
    let parts = [
        selected_raw_baseline_ms(metadata, results)
            .map(|ms| format!("raw compiler floor: {}", format_duration(ms))),
        fbuild_overhead_ms(metadata, results)
            .map(|ms| format!("fbuild overhead: {}", format_duration(ms))),
        fbuild_vs_platformio_cold(results).map(|ratio| format!("fbuild/PIO cold: {ratio:.3}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if parts.is_empty() {
        String::new()
    } else {
        format!(
            "  <text x=\"1076\" y=\"95\" text-anchor=\"end\" class=\"meta\">{}</text>\n",
            xml_escape(&parts.join(" | "))
        )
    }
}

#[expect(clippy::too_many_lines, reason = "baseline, zackees/ci.yml#229")]
fn render_html(metadata: &Metadata, results: &[ToolResult]) -> String {
    let comparison_note = BOARDS
        .iter()
        .map(|board| {
            let status = match board_stack_status(results, board.key) {
                StackStatus::Matched => "matched; fbuild/PlatformIO ratio shown",
                StackStatus::Different => "different; fbuild/PlatformIO ratio excluded",
                StackStatus::Unverified => "unverified; fbuild/PlatformIO ratio excluded",
            };
            format!("{}: {}", board.name, status)
        })
        .collect::<Vec<_>>()
        .join(". ");
    let rows = results
        .iter()
        .map(|result| {
            format!(
                "<tr><th>{}</th><th>{}</th><td>{}</td><td>{}</td><td>{}</td><td>{:.2}x</td><td>{}</td></tr>",
                html_escape(&result.board_name),
                html_escape(&result.display_name),
                format_duration(result.cold_ms),
                format_duration(result.warm_ms),
                result.cold_download_bytes.map_or_else(|| "n/a".to_string(), format_bytes),
                result.speedup,
                html_escape(&result.version)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let phase_rows = results
        .iter()
        .filter(|result| result.tool == "fbuild" || result.tool == "platformio")
        .flat_map(|result| {
            result
                .cold_phases_ms
                .iter()
                .map(move |(phase, ms)| (result, phase, ms))
        })
        .map(|(result, phase, ms)| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                html_escape(&result.board_name),
                html_escape(&result.display_name),
                html_escape(phase),
                format_duration(*ms)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let phase_section = if phase_rows.is_empty() {
        String::new()
    } else {
        format!(
            r#"<h2>Cold phase breakdown</h2>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Board</th><th>Tool</th><th>Phase</th><th>Cold median</th></tr></thead>
          <tbody>{phase_rows}</tbody>
        </table>
      </div>
"#
        )
    };
    let network_note = if metadata.network_mbit > 0 {
        format!(
            "\n      <p class=\"note\">* Measured on a link limited to {} Mbit/s (the benchmark runs in its own shaped network namespace); cold includes every package and toolchain download.</p>",
            metadata.network_mbit
        )
    } else {
        String::new()
    };
    format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>fbuild Blink build benchmark</title>
    <style>
      :root {{ color-scheme: dark; }}
      body {{ margin: 0; padding: 32px 20px 48px; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; color: #f0f6fc; background: #0d1117; }}
      main {{ max-width: 1000px; margin: 0 auto; }}
      h1 {{ margin-bottom: 8px; }}
      a {{ color: #58a6ff; }}
      .meta {{ color: #8b949e; }}
      .note {{ padding: 14px 16px; background: #161b22; border: 1px solid #30363d; border-radius: 6px; }}
      img {{ display: block; width: 100%; height: auto; margin: 24px 0; border: 1px solid #30363d; border-radius: 6px; }}
      .table-wrap {{ overflow-x: auto; }}
      table {{ width: 100%; border-collapse: collapse; background: #161b22; }}
      th, td {{ padding: 10px 12px; border: 1px solid #30363d; text-align: right; }}
      th:first-child, td:first-child, td:last-child {{ text-align: left; }}
      thead th {{ background: #21262d; }}
      code {{ color: #ffa657; }}
    </style>
  </head>
  <body>
    <main>
      <h1>fbuild Blink build benchmark</h1>
      <p class="meta">Generated {generated_at} from <code>{sha}</code>. Median of {trials} trials on {os}/{arch}.</p>
      <p class="note">All three tools compile the same Arduino Uno and ESP32-S3 <code>bench/blink/blink.ino</code>. Cold is a first-time install: each tool's project outputs, caches, packages and toolchains are removed before the timer, so every download lands inside the timed build (for Arduino CLI, that includes <code>arduino-cli core install</code>). Warm is the immediate no-change rebuild. Cold and warm are charted separately, each scaled to its own slowest median, so sub-second warm times stay visible.</p>{network_note}
      <p class="note">Resolved package stacks: {comparison_note}. Exact package versions and representative core compiler commands are in <a href="latest.json">latest.json</a>.</p>
      <a href="benchmark.svg"><img src="benchmark.svg" alt="Arduino CLI vs PlatformIO vs fbuild cold and warm Blink build timings" /></a>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Board</th><th>Tool</th><th>Cold median</th><th>Warm median</th><th>Cold download</th><th>Cold / warm</th><th>Version</th></tr></thead>
          <tbody>{rows}</tbody>
        </table>
      </div>
      {phase_section}<h2>Machine-readable data</h2>
      <ul>
        <li><a href="manifest.json">manifest.json</a> — stable discovery index for agents</li>
        <li><a href="latest.json">latest.json</a> — metadata, raw trials, and medians</li>
        <li><a href="history.jsonl">history.jsonl</a> — rolling 365-run history</li>
      </ul>
      <p><a href="{run_url}">Benchmark workflow run</a> | <a href="https://github.com/{repository}/tree/benchmark-stats">one-commit publication branch</a></p>
    </main>
  </body>
</html>
"#,
        generated_at = html_escape(&metadata.generated_at),
        comparison_note = html_escape(&comparison_note),
        sha = html_escape(&metadata.git_sha),
        trials = metadata.trials,
        os = env::consts::OS,
        arch = env::consts::ARCH,
        rows = rows,
        phase_section = phase_section,
        network_note = network_note,
        run_url = html_escape(&metadata.run_url),
        repository = html_escape(&metadata.repository),
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn html_escape(value: &str) -> String {
    xml_escape(value)
}

fn parse_options() -> AppResult<Options> {
    let mut options = Options {
        output_dir: NormalizedPath::new("benchmark-stats"),
        project_dir: NormalizedPath::new("bench/blink"),
        fbuild: NormalizedPath::new(if cfg!(windows) {
            "target/release/fbuild.exe"
        } else {
            "target/release/fbuild"
        }),
        arduino_cli: OsString::from(if cfg!(windows) {
            "arduino-cli.exe"
        } else {
            "arduino-cli"
        }),
        platformio: OsString::from(if cfg!(windows) { "pio.exe" } else { "pio" }),
        trials: 3,
        network_mbit: 0,
        repository: DEFAULT_REPOSITORY.to_string(),
        git_sha: env::var("GITHUB_SHA").unwrap_or_else(|_| "local".to_string()),
        generated_at: default_generated_at(),
        run_url: String::new(),
        pages_url: DEFAULT_PAGES_URL.to_string(),
        raw_base_url: DEFAULT_RAW_BASE_URL.to_string(),
    };

    let mut args = env::args_os().skip(1);
    while let Some(flag) = args.next() {
        let flag_text = flag.to_string_lossy();
        if flag_text == "--help" || flag_text == "-h" {
            print_help();
            std::process::exit(0);
        }
        let value = args.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("missing value for {flag_text}"),
            )
        })?;
        match flag_text.as_ref() {
            "--output-dir" => options.output_dir = NormalizedPath::new(Path::new(&value)),
            "--project-dir" => options.project_dir = NormalizedPath::new(Path::new(&value)),
            "--fbuild" => options.fbuild = NormalizedPath::new(Path::new(&value)),
            "--arduino-cli" => options.arduino_cli = value,
            "--platformio" => options.platformio = value,
            "--trials" => {
                options.trials = value.to_string_lossy().parse()?;
                if options.trials == 0 || options.trials > 20 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--trials must be between 1 and 20",
                    )
                    .into());
                }
            }
            "--network-mbit" => {
                options.network_mbit = value.to_string_lossy().parse()?;
            }
            "--repository" => options.repository = value.to_string_lossy().into_owned(),
            "--git-sha" => options.git_sha = value.to_string_lossy().into_owned(),
            "--generated-at" => options.generated_at = value.to_string_lossy().into_owned(),
            "--run-url" => options.run_url = value.to_string_lossy().into_owned(),
            "--pages-url" => options.pages_url = value.to_string_lossy().into_owned(),
            "--raw-base-url" => {
                options.raw_base_url = value.to_string_lossy().into_owned();
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown option {flag_text}"),
                )
                .into());
            }
        }
    }
    Ok(options)
}

fn default_generated_at() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("unix:{seconds}")
}

fn print_help() {
    println!(
        "bench-build-comparison [options]\n\
         \n\
         --output-dir PATH       Static-site output (default: benchmark-stats)\n\
         --project-dir PATH      Shared Blink project (default: bench/blink)\n\
         --fbuild PATH           fbuild binary (default: target/release/fbuild)\n\
         --arduino-cli COMMAND   Arduino CLI executable\n\
         --platformio COMMAND    PlatformIO executable\n\
         --trials N              Cold/warm trial pairs (default: 3)\n\
         --network-mbit N        Link rate for disclosure, Mbit/s (default: 0 = unthrottled)\n\
         --repository OWNER/REPO Metadata repository\n\
         --git-sha SHA           Commit measured\n\
         --generated-at ISO8601  Run timestamp\n\
         --run-url URL           GitHub Actions run URL\n\
         --pages-url URL         Published HTML URL\n\
         --raw-base-url URL      Raw benchmark-stats branch base URL"
    );
}

#[cfg(test)]
#[path = "build_comparison_tests.rs"]
mod tests;
