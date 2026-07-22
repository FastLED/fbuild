//! Nightly Arduino CLI vs PlatformIO vs fbuild whole-build benchmark.
//!
//! The harness measures the same Arduino Uno Blink sketch with each real CLI,
//! then renders the one-commit benchmark site's JSON, SVG, and HTML artifacts.

use serde::Serialize;
use serde_json::{Value, json};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const HISTORY_MAX_LINES: usize = 365;
const DEFAULT_REPOSITORY: &str = "FastLED/fbuild";
const DEFAULT_PAGES_URL: &str = "https://fastled.github.io/fbuild/";
const DEFAULT_RAW_BASE_URL: &str =
    "https://raw.githubusercontent.com/FastLED/fbuild/benchmark-stats";

type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct Options {
    output_dir: PathBuf,
    project_dir: PathBuf,
    fbuild: PathBuf,
    arduino_cli: OsString,
    platformio: OsString,
    trials: usize,
    repository: String,
    git_sha: String,
    generated_at: String,
    run_url: String,
    pages_url: String,
    raw_base_url: String,
}

#[derive(Clone, Copy, Debug)]
enum ToolKind {
    Arduino,
    PlatformIo,
    Fbuild,
}

#[derive(Clone, Copy, Debug)]
struct ToolStyle {
    key: &'static str,
    label: &'static str,
    cold_color: &'static str,
    warm_color: &'static str,
}

impl ToolKind {
    fn style(self) -> ToolStyle {
        match self {
            Self::Arduino => ToolStyle {
                key: "arduino",
                label: "Arduino CLI",
                cold_color: "#3b4046",
                warm_color: "#8b949e",
            },
            Self::PlatformIo => ToolStyle {
                key: "platformio",
                label: "PlatformIO",
                cold_color: "#1f3a7a",
                warm_color: "#79c0ff",
            },
            Self::Fbuild => ToolStyle {
                key: "fbuild",
                label: "fbuild",
                cold_color: "#5b1f1c",
                warm_color: "#f85149",
            },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct ToolResult {
    tool: String,
    display_name: String,
    version: String,
    cold_ms: f64,
    warm_ms: f64,
    speedup: f64,
    cold_trials_ms: Vec<f64>,
    warm_trials_ms: Vec<f64>,
}

#[derive(Clone, Debug)]
struct Metadata {
    generated_at: String,
    git_sha: String,
    repository: String,
    run_url: String,
    project: String,
    trials: usize,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("build comparison benchmark failed: {error}");
        std::process::exit(1);
    }
}

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

    let arduino_build_dir = repo_root.join("benchmark-output/arduino-build");
    let mut results = Vec::with_capacity(versions.len());
    for (kind, version) in versions {
        let result = measure_tool(
            kind,
            &version,
            &options,
            &repo_root,
            &project_dir,
            &fbuild,
            &arduino_build_dir,
            &mut log,
        )?;
        println!(
            "{:<12} cold {:>10.3} ms | warm {:>10.3} ms | {:>7.2}x",
            result.display_name, result.cold_ms, result.warm_ms, result.speedup
        );
        results.push(result);
    }

    let metadata = Metadata {
        generated_at: options.generated_at.clone(),
        git_sha: options.git_sha.clone(),
        repository: options.repository.clone(),
        run_url: options.run_url.clone(),
        project: options.project_dir.to_string_lossy().replace('\\', "/"),
        trials: options.trials,
    };
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
fn measure_tool(
    kind: ToolKind,
    version: &str,
    options: &Options,
    repo_root: &Path,
    project_dir: &Path,
    fbuild: &Path,
    arduino_build_dir: &Path,
    log: &mut File,
) -> AppResult<ToolResult> {
    let mut cold_trials_ms = Vec::with_capacity(options.trials);
    let mut warm_trials_ms = Vec::with_capacity(options.trials);

    if matches!(kind, ToolKind::Fbuild) {
        writeln!(log, "\n===== fbuild daemon preflight =====")?;
        run_logged(
            fbuild.as_os_str(),
            &os_args(&["daemon", "restart"]),
            repo_root,
            log,
        )?;
    }

    for trial in 1..=options.trials {
        writeln!(
            log,
            "\n===== {} trial {trial}/{} =====",
            kind.style().label,
            options.trials
        )?;
        clean_tool(
            kind,
            options,
            repo_root,
            project_dir,
            fbuild,
            arduino_build_dir,
            log,
        )?;
        let cold = timed_build(
            kind,
            options,
            repo_root,
            project_dir,
            fbuild,
            arduino_build_dir,
            log,
        )?;
        let warm = timed_build(
            kind,
            options,
            repo_root,
            project_dir,
            fbuild,
            arduino_build_dir,
            log,
        )?;
        cold_trials_ms.push(round_millis(cold));
        warm_trials_ms.push(round_millis(warm));
    }

    let cold_ms = round_millis(median(&cold_trials_ms));
    let warm_ms = round_millis(median(&warm_trials_ms));
    let speedup = if warm_ms > 0.0 {
        round_to(cold_ms / warm_ms, 3)
    } else {
        0.0
    };
    let style = kind.style();
    Ok(ToolResult {
        tool: style.key.to_string(),
        display_name: style.label.to_string(),
        version: version.to_string(),
        cold_ms,
        warm_ms,
        speedup,
        cold_trials_ms,
        warm_trials_ms,
    })
}

fn clean_tool(
    kind: ToolKind,
    options: &Options,
    repo_root: &Path,
    project_dir: &Path,
    fbuild: &Path,
    arduino_build_dir: &Path,
    log: &mut File,
) -> AppResult<()> {
    match kind {
        ToolKind::Arduino => remove_dir_within(repo_root, arduino_build_dir),
        ToolKind::PlatformIo => {
            let args = os_args(&[
                "run",
                "--project-dir",
                &project_dir.to_string_lossy(),
                "--environment",
                "uno",
                "--target",
                "clean",
            ]);
            run_logged(&options.platformio, &args, repo_root, log).map(|_| ())
        }
        ToolKind::Fbuild => {
            let args = os_args(&[
                "clean",
                "all",
                &project_dir.to_string_lossy(),
                "--environment",
                "uno",
                "--release",
            ]);
            run_logged(fbuild.as_os_str(), &args, repo_root, log).map(|_| ())
        }
    }
}

fn timed_build(
    kind: ToolKind,
    options: &Options,
    repo_root: &Path,
    project_dir: &Path,
    fbuild: &Path,
    arduino_build_dir: &Path,
    log: &mut File,
) -> AppResult<f64> {
    let (program, args) = match kind {
        ToolKind::Arduino => (
            options.arduino_cli.as_os_str(),
            os_args(&[
                "compile",
                "--fqbn",
                "arduino:avr:uno",
                "--build-path",
                &arduino_build_dir.to_string_lossy(),
                &project_dir.to_string_lossy(),
            ]),
        ),
        ToolKind::PlatformIo => (
            options.platformio.as_os_str(),
            os_args(&[
                "run",
                "--project-dir",
                &project_dir.to_string_lossy(),
                "--environment",
                "uno",
            ]),
        ),
        ToolKind::Fbuild => (
            fbuild.as_os_str(),
            os_args(&[
                "build",
                &project_dir.to_string_lossy(),
                "--environment",
                "uno",
                "--release",
            ]),
        ),
    };

    let started = Instant::now();
    run_logged(program, &args, repo_root, log)?;
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

fn os_args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn run_logged(program: &OsStr, args: &[OsString], cwd: &Path, log: &mut File) -> AppResult<Output> {
    writeln!(log, "$ {}", display_command(program, args))?;
    log.flush()?;
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("CI", "true")
        .env("PLATFORMIO_SETTING_ENABLE_TELEMETRY", "no")
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

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
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
    write_json(output_dir.join("latest.json"), &latest)?;
    write_history(output_dir.join("history.jsonl"), metadata, results)?;
    write_json(
        output_dir.join("manifest.json"),
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
        "schema_version": 1,
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
            "board": "Arduino Uno",
            "fqbn": "arduino:avr:uno",
            "toolchain_pins": {
                "arduino_core": "arduino:avr@1.8.8",
                "platformio_platform": "atmelavr@5.1.0",
                "note": "ecosystem framework distributions are pinned independently",
            },
            "trials": metadata.trials,
            "statistic": "median",
            "cold_definition": "project outputs and matching compiled framework caches removed; installed packages and global download/compiler caches retained",
            "warm_definition": "immediate no-change rebuild after the cold build",
        },
        "results": results,
    })
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
                "schema_version": 1,
            },
            "history": {
                "description": "Rolling compact history, one JSON object per benchmark run.",
                "url": format!("{raw}/history.jsonl"),
                "content_type": "application/x-ndjson",
                "schema_version": 1,
                "max_lines": HISTORY_MAX_LINES,
            },
            "image": {
                "description": "GitHub-dark cold bars with narrower warm timing overlays.",
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

fn write_json(path: PathBuf, payload: &Value) -> AppResult<()> {
    fs::write(path, serde_json::to_string_pretty(payload)? + "\n")?;
    Ok(())
}

fn write_history(path: PathBuf, metadata: &Metadata, results: &[ToolResult]) -> AppResult<()> {
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
        "results": compact_results,
    }))?);
    fs::write(path, prior.join("\n") + "\n")?;
    Ok(())
}

fn render_svg(metadata: &Metadata, results: &[ToolResult]) -> String {
    let width = 960.0;
    let height = 410.0;
    let bar_x = 190.0;
    let bar_width = 480.0;
    let max_ms = results
        .iter()
        .flat_map(|result| [result.cold_ms, result.warm_ms])
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let mut rows = String::new();
    for (index, result) in results.iter().enumerate() {
        let kind = match result.tool.as_str() {
            "arduino" => ToolKind::Arduino,
            "platformio" => ToolKind::PlatformIo,
            _ => ToolKind::Fbuild,
        };
        let style = kind.style();
        let y = 174.0 + index as f64 * 68.0;
        let cold_width = (result.cold_ms / max_ms * bar_width).max(3.0);
        let warm_width = (result.warm_ms / max_ms * bar_width).max(3.0);
        rows.push_str(&format!(
            r##"  <g>
    <text x="24" y="{label_y:.1}" class="tool" fill="{warm}">{label}</text>
    <rect x="{bar_x:.1}" y="{cold_y:.1}" width="{bar_width:.1}" height="28" rx="4" fill="#21262d" />
    <rect x="{bar_x:.1}" y="{cold_y:.1}" width="{cold_width:.1}" height="28" rx="4" fill="{cold}">
      <title>{label} cold median: {cold_ms:.3} ms</title>
    </rect>
    <rect x="{bar_x:.1}" y="{warm_y:.1}" width="{warm_width:.1}" height="14" rx="3" fill="{warm}">
      <title>{label} warm median: {warm_ms:.3} ms</title>
    </rect>
    <text x="692" y="{value_y:.1}" class="value">cold {cold_ms:.1} ms  |  warm {warm_ms:.1} ms</text>
    <text x="692" y="{speed_y:.1}" class="speed">{speedup:.2}x cold / warm</text>
  </g>
"##,
            label_y = y + 21.0,
            label = xml_escape(&result.display_name),
            warm = style.warm_color,
            bar_x = bar_x,
            cold_y = y,
            bar_width = bar_width,
            cold_width = cold_width,
            cold = style.cold_color,
            cold_ms = result.cold_ms,
            warm_y = y + 7.0,
            warm_width = warm_width,
            warm_ms = result.warm_ms,
            value_y = y + 13.0,
            speed_y = y + 34.0,
            speedup = result.speedup,
        ));
    }
    let short_sha = metadata.git_sha.chars().take(12).collect::<String>();
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width:.0}" height="{height:.0}" viewBox="0 0 {width:.0} {height:.0}" role="img" aria-labelledby="title description">
  <title id="title">Arduino CLI vs PlatformIO vs fbuild Blink build benchmark</title>
  <desc id="description">Cold build bars with narrower warm build timing overlays for an Arduino Uno Blink sketch.</desc>
  <style>
    text {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
    .heading {{ font-size: 30px; font-weight: 700; fill: #f0f6fc; }}
    .meta {{ font-size: 14px; fill: #8b949e; }}
    .legend {{ font-size: 14px; font-weight: 600; fill: #c9d1d9; }}
    .tool {{ font-size: 18px; font-weight: 700; }}
    .value {{ font-size: 14px; font-weight: 600; fill: #f0f6fc; }}
    .speed {{ font-size: 13px; fill: #8b949e; }}
  </style>
  <rect width="{width:.0}" height="{height:.0}" fill="#0d1117" />
  <rect width="{width:.0}" height="112" fill="#161b22" />
  <text x="24" y="43" class="heading">Arduino Uno Blink build times</text>
  <text x="24" y="72" class="meta">Arduino CLI vs PlatformIO vs fbuild | median of {trials} trials</text>
  <text x="24" y="95" class="meta">Generated {generated_at} | sha {sha}</text>
  <rect x="24" y="128" width="76" height="24" rx="4" fill="#5b1f1c" />
  <rect x="24" y="134" width="34" height="12" rx="3" fill="#f85149" />
  <text x="112" y="146" class="legend">cold (back) + warm (front overlay)</text>
  <text x="692" y="146" class="meta">scale: slowest median = {max_ms:.1} ms</text>
{rows}  <line x1="24" y1="382" x2="936" y2="382" stroke="#30363d" stroke-width="2" />
  <text x="24" y="402" class="meta">Machine data: manifest.json | latest.json | history.jsonl</text>
</svg>
"##,
        width = width,
        height = height,
        trials = metadata.trials,
        generated_at = xml_escape(&metadata.generated_at),
        sha = xml_escape(&short_sha),
        max_ms = max_ms,
        rows = rows,
    )
}

fn render_html(metadata: &Metadata, results: &[ToolResult]) -> String {
    let rows = results
        .iter()
        .map(|result| {
            format!(
                "<tr><th>{}</th><td>{:.3} ms</td><td>{:.3} ms</td><td>{:.2}x</td><td>{}</td></tr>",
                html_escape(&result.display_name),
                result.cold_ms,
                result.warm_ms,
                result.speedup,
                html_escape(&result.version)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
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
      <p class="note">All three tools compile the same Arduino Uno <code>bench/blink/blink.ino</code>. Cold removes project outputs and matching compiled framework caches while retaining installed packages and global download/compiler caches. Warm is the immediate no-change rebuild. The narrower warm bar overlays the cold bar.</p>
      <a href="benchmark.svg"><img src="benchmark.svg" alt="Arduino CLI vs PlatformIO vs fbuild cold and warm Blink build timings" /></a>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Tool</th><th>Cold median</th><th>Warm median</th><th>Cold / warm</th><th>Version</th></tr></thead>
          <tbody>{rows}</tbody>
        </table>
      </div>
      <h2>Machine-readable data</h2>
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
        sha = html_escape(&metadata.git_sha),
        trials = metadata.trials,
        os = env::consts::OS,
        arch = env::consts::ARCH,
        rows = rows,
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
        output_dir: PathBuf::from("benchmark-stats"),
        project_dir: PathBuf::from("bench/blink"),
        fbuild: PathBuf::from(if cfg!(windows) {
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
            "--output-dir" => options.output_dir = PathBuf::from(value),
            "--project-dir" => options.project_dir = PathBuf::from(value),
            "--fbuild" => options.fbuild = PathBuf::from(value),
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
         --repository OWNER/REPO Metadata repository\n\
         --git-sha SHA           Commit measured\n\
         --generated-at ISO8601  Run timestamp\n\
         --run-url URL           GitHub Actions run URL\n\
         --pages-url URL         Published HTML URL\n\
         --raw-base-url URL      Raw benchmark-stats branch base URL"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_results() -> Vec<ToolResult> {
        vec![
            ToolResult {
                tool: "arduino".into(),
                display_name: "Arduino CLI".into(),
                version: "arduino-cli 1.5.0".into(),
                cold_ms: 1200.0,
                warm_ms: 800.0,
                speedup: 1.5,
                cold_trials_ms: vec![1100.0, 1200.0, 1300.0],
                warm_trials_ms: vec![750.0, 800.0, 850.0],
            },
            ToolResult {
                tool: "platformio".into(),
                display_name: "PlatformIO".into(),
                version: "PlatformIO Core 6.1.19".into(),
                cold_ms: 900.0,
                warm_ms: 300.0,
                speedup: 3.0,
                cold_trials_ms: vec![850.0, 900.0, 950.0],
                warm_trials_ms: vec![280.0, 300.0, 320.0],
            },
            ToolResult {
                tool: "fbuild".into(),
                display_name: "fbuild".into(),
                version: "fbuild 0.1.0".into(),
                cold_ms: 600.0,
                warm_ms: 40.0,
                speedup: 15.0,
                cold_trials_ms: vec![580.0, 600.0, 620.0],
                warm_trials_ms: vec![38.0, 40.0, 42.0],
            },
        ]
    }

    fn sample_metadata() -> Metadata {
        Metadata {
            generated_at: "2026-07-22T12:00:00Z".into(),
            git_sha: "0123456789abcdef".into(),
            repository: DEFAULT_REPOSITORY.into(),
            run_url: "https://github.com/FastLED/fbuild/actions/runs/1".into(),
            project: "bench/blink".into(),
            trials: 3,
        }
    }

    #[test]
    fn median_handles_odd_and_even_trial_counts() {
        assert_eq!(median(&[9.0, 1.0, 5.0]), 5.0);
        assert_eq!(median(&[9.0, 1.0, 7.0, 3.0]), 5.0);
    }

    #[test]
    fn remove_dir_within_guards_boundaries() {
        let sandbox = tempfile::tempdir().unwrap();
        let root = sandbox.path().join("root");
        let nested = root.join("nested");
        let sibling = sandbox.path().join("sibling");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&sibling).unwrap();

        assert!(remove_dir_within(&root, &root).is_err());
        assert!(remove_dir_within(&root, &sibling).is_err());
        assert!(root.is_dir());
        assert!(sibling.is_dir());

        remove_dir_within(&root, &nested).unwrap();
        assert!(!nested.exists());
    }

    #[test]
    fn svg_uses_reference_palette_and_warm_overlay() {
        let svg = render_svg(&sample_metadata(), &sample_results());
        for color in [
            "#3b4046", "#8b949e", "#1f3a7a", "#79c0ff", "#5b1f1c", "#f85149",
        ] {
            assert!(svg.contains(color), "missing {color}");
        }
        assert!(svg.contains("height=\"28\""));
        assert!(svg.contains("height=\"14\""));
        assert!(svg.contains("cold (back) + warm (front overlay)"));
    }

    #[test]
    fn outputs_include_agent_discovery_and_bounded_history() {
        let temp = tempfile::tempdir().unwrap();
        let history = (0..HISTORY_MAX_LINES)
            .map(|index| format!(r#"{{"old":{index}}}"#))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(temp.path().join("history.jsonl"), history).unwrap();
        write_outputs(
            temp.path(),
            &sample_metadata(),
            &sample_results(),
            DEFAULT_PAGES_URL,
            DEFAULT_RAW_BASE_URL,
        )
        .unwrap();

        for file in [
            "manifest.json",
            "latest.json",
            "history.jsonl",
            "benchmark.svg",
            "index.html",
            ".nojekyll",
        ] {
            assert!(temp.path().join(file).is_file(), "missing {file}");
        }
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(temp.path().join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["branch"], "benchmark-stats");
        assert_eq!(
            manifest["artifacts"]["history"]["max_lines"],
            HISTORY_MAX_LINES
        );
        let history = fs::read_to_string(temp.path().join("history.jsonl")).unwrap();
        assert_eq!(history.lines().count(), HISTORY_MAX_LINES);
        assert!(history.lines().last().unwrap().contains("0123456789abcdef"));
        let html = fs::read_to_string(temp.path().join("index.html")).unwrap();
        assert!(html.contains("stable discovery index for agents"));
    }
}
