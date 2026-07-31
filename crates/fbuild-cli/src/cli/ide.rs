//! `fbuild ide` / `fbuild ide select`: open (or configure) a project as an
//! IDE workspace on stock Zed (FastLED/fbuild#1076 Phase 1).
//!
//! This is a thin orchestrator over machinery that already exists:
//!
//! - Environment resolution / compile-DB freshness / `.clangd` + per-editor
//!   config emission all come from the editor-neutral core in
//!   `clangd_config` (`ensure_compile_db`, `emit_clangd_file`,
//!   `emit_editor_config`) — FastLED/fbuild#1076 Phase 0.
//! - Declared-dep install goes through the daemon's existing
//!   `POST /api/install-deps` handler, exactly like a fresh `fbuild build`
//!   would trigger via the framework/library installer.
//!
//! What's new here: a persisted per-project "which environment is the IDE
//! configured for" choice (`.fbuild/ide_state.json`), an fbuild-owned
//! `.zed/tasks.json` (merge-don't-clobber: fbuild only touches tasks whose
//! label starts with `"fbuild: "`), and launching the `zed` process.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::daemon_client::{self, DaemonClient, InstallDepsRequest};
use crate::output;

use super::build::normalize_path;
use super::clangd_config::{Editor, emit_clangd_file, emit_editor_config, ensure_compile_db};
use super::ide_debug::{self, PROBE_RS_DAP_PORT, probe_rs_chip_for_mcu, unsupported_debug_note};

/// Label prefix that marks a Zed task as fbuild-owned. Merge logic replaces
/// every task with this prefix and leaves everything else untouched.
const FBUILD_TASK_PREFIX: &str = "fbuild: ";

// ---------------------------------------------------------------------
// Persisted IDE state
// ---------------------------------------------------------------------

/// Persisted per-project IDE state: `<project>/.fbuild/ide_state.json`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct IdeState {
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
}

fn ide_state_path(project_path: &Path) -> PathBuf {
    project_path.join(".fbuild").join("ide_state.json")
}

/// Read the persisted environment. Tolerates an absent file, an empty file,
/// or malformed JSON — all of those degrade to `None` rather than erroring,
/// since a missing/corrupt state file just means "fall through to the next
/// resolution step", never a hard failure.
fn read_persisted_env(project_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(ide_state_path(project_path)).ok()?;
    let state: IdeState = serde_json::from_str(&content).ok()?;
    state.environment
}

/// Persist the chosen environment, creating `.fbuild/` if needed.
fn write_persisted_env(project_path: &Path, environment: &str) -> fbuild_core::Result<()> {
    let dir = project_path.join(".fbuild");
    std::fs::create_dir_all(&dir).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to create {}: {}", dir.display(), e))
    })?;
    let state = IdeState {
        environment: Some(environment.to_string()),
    };
    let mut json = serde_json::to_string_pretty(&state).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to serialize ide state: {}", e))
    })?;
    json.push('\n');
    let path = ide_state_path(project_path);
    std::fs::write(&path, json).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to write {}: {}", path.display(), e))
    })?;
    Ok(())
}

/// Resolve the environment to configure the IDE for: explicit `-e` wins,
/// then the persisted choice, then `platformio.ini`'s default environment.
fn resolve_ide_env(project_path: &Path, explicit: Option<String>) -> fbuild_core::Result<String> {
    if let Some(env) = explicit {
        return Ok(env);
    }
    if let Some(env) = read_persisted_env(project_path) {
        return Ok(env);
    }
    let ini_path = project_path.join("platformio.ini");
    if !ini_path.exists() {
        return Err(fbuild_core::FbuildError::ConfigError(format!(
            "no platformio.ini found at {}",
            ini_path.display()
        )));
    }
    let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
    config
        .get_default_environment()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError(
                "no environments defined in platformio.ini".into(),
            )
        })
}

// ---------------------------------------------------------------------
// .zed/tasks.json — merge-don't-clobber
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ZedTask {
    label: String,
    command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
}

/// The fbuild-owned tasks for a given environment. All but the debug-server
/// task are plain `fbuild` invocations so they render in Zed's terminal
/// panel with clickable `file:line` diagnostics, same as running them by
/// hand.
///
/// `debug_chip`, when `Some` (i.e. [`probe_rs_chip_for_mcu`] resolved a
/// probe-rs chip for the current environment — FastLED/fbuild#1076 Phase
/// 3), adds a task that runs `probe-rs dap-server` on
/// [`PROBE_RS_DAP_PORT`], which `.zed/debug.json`'s attach entry connects
/// to. fbuild does not install probe-rs itself in milestone 1 — if it's
/// missing, the task fails in Zed's terminal panel with probe-rs's own
/// "command not found" message.
fn build_fbuild_tasks(env_name: &str, debug_chip: Option<&str>) -> Vec<ZedTask> {
    let task = |label: &str, command: &str, args: Vec<String>| ZedTask {
        label: format!("{FBUILD_TASK_PREFIX}{label}"),
        command: command.to_string(),
        args,
    };
    let str_args = |args: Vec<&str>| args.into_iter().map(str::to_string).collect::<Vec<_>>();
    let mut tasks = vec![
        task("Build", "fbuild", str_args(vec!["build", "-e", env_name])),
        task(
            "Build (clean)",
            "fbuild",
            str_args(vec!["build", "-e", env_name, "--clean"]),
        ),
        task("Deploy", "fbuild", str_args(vec!["deploy", "-e", env_name])),
        task(
            "Deploy + Monitor",
            "fbuild",
            str_args(vec!["deploy", "-e", env_name, "--monitor"]),
        ),
        task(
            "Monitor",
            "fbuild",
            str_args(vec!["monitor", "-e", env_name]),
        ),
        task("Reset", "fbuild", str_args(vec!["reset", "-e", env_name])),
        // Opens the daemon-served Serial Plotter page in the default
        // browser (FastLED/fbuild#1076 Phase 2). No port pinned here --
        // the page's own port selector (populated from
        // /api/devices/list) is how the user picks a port, so this task
        // works regardless of which environment/port is active.
        task("Serial Plotter", "fbuild", str_args(vec!["plotter"])),
        // Opens the daemon-served Build Progress page (FastLED/fbuild#1076
        // Phase 2, second panel): status polled from /api/daemon/info,
        // activity tail over the existing /ws/logs broadcast websocket.
        task("Build Progress", "fbuild", str_args(vec!["build-progress"])),
        // Opens the daemon-served Board Manager page (FastLED/fbuild#1076
        // Phase 2, third/fourth panels): read-only, searchable table over
        // fbuild's embedded board database. Daemon-global, no env needed.
        task("Board Manager", "fbuild", str_args(vec!["boards"])),
        // Opens the daemon-served Library Manager page for this
        // environment's declared lib_deps, classified + annotated with
        // best-effort install state.
        task(
            "Library Manager",
            "fbuild",
            str_args(vec!["libraries", "-e", env_name]),
        ),
        task(
            "Select environment",
            "fbuild",
            str_args(vec!["ide", "select"]),
        ),
    ];
    if let Some(chip) = debug_chip {
        tasks.push(task(
            "Debug server (probe-rs)",
            "probe-rs",
            vec![
                "dap-server".to_string(),
                "--port".to_string(),
                PROBE_RS_DAP_PORT.to_string(),
                "--chip".to_string(),
                chip.to_string(),
            ],
        ));
    }
    tasks
}

/// Merge fbuild's tasks into `.zed/tasks.json`: any existing task whose
/// label starts with `"fbuild: "` is replaced (by position among the
/// fbuild-owned tasks); every other (user) task is preserved verbatim, in
/// its original position.
fn merge_tasks(existing: &[ZedTask], fbuild_tasks: &[ZedTask]) -> Vec<ZedTask> {
    let mut merged: Vec<ZedTask> = existing
        .iter()
        .filter(|t| !t.label.starts_with(FBUILD_TASK_PREFIX))
        .cloned()
        .collect();
    merged.extend(fbuild_tasks.iter().cloned());
    merged
}

fn read_tasks_file(path: &Path) -> Vec<ZedTask> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    if content.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str(&content).unwrap_or_default()
}

/// Write `.zed/tasks.json` with fbuild's tasks merged in, preserving any
/// user-authored tasks. Returns the path written.
fn emit_zed_tasks(
    project_path: &Path,
    env_name: &str,
    debug_chip: Option<&str>,
) -> fbuild_core::Result<PathBuf> {
    let zed_dir = project_path.join(".zed");
    std::fs::create_dir_all(&zed_dir).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to create {}: {}", zed_dir.display(), e))
    })?;
    let tasks_path = zed_dir.join("tasks.json");
    let existing = read_tasks_file(&tasks_path);
    let merged = merge_tasks(&existing, &build_fbuild_tasks(env_name, debug_chip));
    let mut json = serde_json::to_string_pretty(&merged).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to serialize tasks.json: {}", e))
    })?;
    json.push('\n');
    std::fs::write(&tasks_path, json).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to write {}: {}", tasks_path.display(), e))
    })?;
    Ok(tasks_path)
}

// ---------------------------------------------------------------------
// zed executable discovery + launch
// ---------------------------------------------------------------------

/// Known install-location candidates for the `zed` executable, beyond
/// PATH, in probe order. Pure (no filesystem access) so it's directly
/// testable; callers are responsible for checking `.exists()`.
fn known_zed_install_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if cfg!(windows) {
        if let Some(local_appdata) = std::env::var_os("LOCALAPPDATA") {
            let base = PathBuf::from(local_appdata);
            candidates.push(base.join("Programs").join("Zed").join("zed.exe"));
            candidates.push(base.join("Zed").join("zed.exe"));
        }
    } else if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from("/Applications/Zed.app/Contents/MacOS/cli"));
        candidates.push(PathBuf::from("/usr/local/bin/zed"));
    } else {
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join(".local").join("bin").join("zed"));
        }
        candidates.push(PathBuf::from("/usr/bin/zed"));
    }
    candidates
}

fn zed_exe_name() -> &'static str {
    if cfg!(windows) { "zed.exe" } else { "zed" }
}

/// Find `zed` on PATH first, then fall back to known install locations.
fn find_zed_executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        let exe_name = zed_exe_name();
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    known_zed_install_candidates()
        .into_iter()
        .find(|p| p.is_file())
}

fn print_zed_install_guidance() {
    output::result("");
    output::result("Zed was not found on PATH or in known install locations.");
    output::result(
        "IDE config was still generated — install Zed and open the project manually, or install it and re-run `fbuild ide`:",
    );
    output::result("  Windows: winget install Zed.Zed");
    output::result("  macOS:   brew install --cask zed");
    output::result("  Any OS:  https://zed.dev/download");
}

/// Spawn `zed <project_dir>` detached (fire-and-forget — the CLI does not
/// wait on the editor process).
fn launch_zed(zed_path: &Path, project_dir: &str) -> fbuild_core::Result<()> {
    // allow-direct-spawn: editor is launched detached and must outlive the CLI; deliberately not in a containment group.
    let mut cmd = std::process::Command::new(zed_path);
    cmd.arg(project_dir);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const DETACHED_PROCESS: u32 = 0x00000008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    cmd.spawn()
        .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to launch zed: {}", e)))?;
    Ok(())
}

// ---------------------------------------------------------------------
// debug.json (FastLED/fbuild#1076 Phase 3, milestone 1: probe-rs targets)
// ---------------------------------------------------------------------

/// Resolve the `BoardConfig::mcu` string for `env_name`, the same way
/// `deploy::infer_cli_default_emulator_kind` resolves a board for emulator
/// hints: `platform =` / `board =` from `platformio.ini`, plus any
/// `board_build.*`/`board_upload.*` overrides, resolved against the
/// built-in board DB (falling back to `<project>/boards/<id>.json`).
/// Best-effort — returns `Ok(None)` (never an error) when the environment
/// has no resolvable board, since a missing board just means "no debug
/// config", not a hard failure.
fn resolve_mcu_for_env(project_path: &Path, env_name: &str) -> fbuild_core::Result<Option<String>> {
    let ini_path = project_path.join("platformio.ini");
    let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
    let Ok(env_config) = config.get_env_config(env_name) else {
        return Ok(None);
    };
    let Some(board_id) = env_config.get("board") else {
        return Ok(None);
    };
    let board_overrides = config.get_board_overrides(env_name).unwrap_or_default();
    let board = fbuild_config::BoardConfig::from_board_id_with_override_fallback(
        board_id,
        &board_overrides,
        Some(project_path),
    );
    Ok(board.map(|b| b.mcu))
}

/// Resolve the probe-rs chip (if any) for `env_name` and, when found, emit
/// `.zed/debug.json`. Returns `(chip, debug_json_path)` — `chip` feeds the
/// `.zed/tasks.json` debug-server task, `debug_json_path` feeds the
/// generated-files summary. When the environment's board/MCU isn't a
/// probe-rs target, prints the one-line unsupported note and returns
/// `(None, None)` — this is a first-class outcome, not an error.
fn regenerate_debug_config(
    project_path: &Path,
    env_name: &str,
) -> fbuild_core::Result<(Option<String>, Option<PathBuf>)> {
    let mcu = resolve_mcu_for_env(project_path, env_name)?;
    let Some(chip) = mcu.as_deref().and_then(probe_rs_chip_for_mcu) else {
        let label = mcu.as_deref().unwrap_or(env_name);
        output::result(unsupported_debug_note(label));
        return Ok((None, None));
    };
    let elf_path = ide_debug::expected_elf_path(project_path, env_name);
    let debug_path = ide_debug::emit_zed_debug(project_path, env_name, Some(&elf_path))?;
    Ok((Some(chip.to_string()), Some(debug_path)))
}

// ---------------------------------------------------------------------
// daemon-backed steps
// ---------------------------------------------------------------------

/// Install declared platform/framework/library deps via the daemon, same
/// contract as `POST /api/install-deps` (`fbuild_daemon::models::InstallDepsRequest`).
async fn install_declared_deps(project_dir: &str, env_name: &str) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();
    let (caller_pid, caller_cwd) = daemon_client::caller_info();
    let req = InstallDepsRequest {
        project_dir: project_dir.to_string(),
        environment: Some(env_name.to_string()),
        request_id: None,
        caller_pid,
        caller_cwd,
    };
    output::progress("Installing declared dependencies...");
    let resp = client.install_deps(&req).await?;
    if !resp.success {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "install-deps failed: {}",
            resp.message
        )));
    }
    Ok(())
}

/// Regenerate compile DB + `.clangd` + `.zed/settings.json` +
/// `.zed/tasks.json` for `env_name`. Shared by `run_ide` and
/// `run_ide_select` so both paths refresh identically.
async fn regenerate_ide_config(
    project_dir: &str,
    project_path: &Path,
    env_name: &str,
    verbose: bool,
) -> fbuild_core::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let db_path = ensure_compile_db(project_dir, project_path, env_name, verbose, true).await?;
    written.push(db_path);
    written.push(emit_clangd_file(project_path)?);
    for (path, _written) in emit_editor_config(Editor::Zed, project_path)? {
        written.push(path);
    }

    let (debug_chip, debug_path) = regenerate_debug_config(project_path, env_name)?;
    if let Some(debug_path) = debug_path {
        written.push(debug_path);
    }
    written.push(emit_zed_tasks(
        project_path,
        env_name,
        debug_chip.as_deref(),
    )?);
    Ok(written)
}

// ---------------------------------------------------------------------
// public entry points
// ---------------------------------------------------------------------

/// `fbuild ide [project_dir] [-e <environment>] [--no-launch]`
pub async fn run_ide(
    project_dir: String,
    environment: Option<String>,
    no_launch: bool,
) -> fbuild_core::Result<()> {
    let project_dir = normalize_path(&project_dir).await?;
    let project_path = Path::new(&project_dir);

    let env_name = resolve_ide_env(project_path, environment)?;
    output::progress(format!("Using environment: {}", env_name));
    write_persisted_env(project_path, &env_name)?;

    install_declared_deps(&project_dir, &env_name).await?;

    let written = regenerate_ide_config(&project_dir, project_path, &env_name, false).await?;

    output::result("\nGenerated IDE configuration:");
    for path in &written {
        output::result(format!("  {}", path.display()));
    }

    if no_launch {
        output::result("\n--no-launch: skipping Zed launch.");
        return Ok(());
    }

    match find_zed_executable() {
        Some(zed_path) => {
            output::progress(format!("Launching Zed ({})...", zed_path.display()));
            launch_zed(&zed_path, &project_dir)?;
        }
        None => print_zed_install_guidance(),
    }

    Ok(())
}

/// `fbuild ide select [project_dir] [-e <environment>]`
///
/// Interactively pick the environment (unless `-e` bypasses the prompt),
/// persist it, and regenerate the compile DB + `.clangd` + `.zed/*` config
/// for it.
pub async fn run_ide_select(
    project_dir: String,
    environment: Option<String>,
) -> fbuild_core::Result<()> {
    let project_dir = normalize_path(&project_dir).await?;
    let project_path = Path::new(&project_dir);

    let ini_path = project_path.join("platformio.ini");
    if !ini_path.exists() {
        return Err(fbuild_core::FbuildError::ConfigError(format!(
            "no platformio.ini found at {}",
            ini_path.display()
        )));
    }
    let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
    let mut envs: Vec<String> = config
        .get_environments()
        .into_iter()
        .map(str::to_string)
        .collect();
    envs.sort();
    if envs.is_empty() {
        return Err(fbuild_core::FbuildError::ConfigError(
            "no environments defined in platformio.ini".into(),
        ));
    }

    let chosen = match environment {
        Some(env) => {
            if !config.has_environment(&env) {
                return Err(fbuild_core::FbuildError::ConfigError(format!(
                    "unknown environment '{}' — available: {}",
                    env,
                    envs.join(", ")
                )));
            }
            env
        }
        None => prompt_env_choice(&envs)?,
    };

    write_persisted_env(project_path, &chosen)?;
    output::progress(format!("Selected environment: {}", chosen));

    regenerate_ide_config(&project_dir, project_path, &chosen, false).await?;
    output::result(format!(
        "\nIDE configuration regenerated for environment '{}'.",
        chosen
    ));
    Ok(())
}

/// Interactive numbered picker, modeled on `sync::prompt_multi_env`: reads
/// a single line from stdin, writes prompts to stderr so stdout stays
/// clean for pipelines.
fn prompt_env_choice(envs: &[String]) -> fbuild_core::Result<String> {
    use std::io::{BufRead, Write};
    eprintln!("Select the environment for this project's IDE config:");
    for (idx, env) in envs.iter().enumerate() {
        eprintln!("  {}) {}", idx + 1, env);
    }
    eprint!("Enter a number [1-{}]: ", envs.len());
    let _ = std::io::stderr().flush();
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to read selection: {}", e)))?;
    let choice: usize = line.trim().parse().map_err(|_| {
        fbuild_core::FbuildError::Other(format!("'{}' is not a valid selection", line.trim()))
    })?;
    envs.get(choice.wrapping_sub(1))
        .cloned()
        .ok_or_else(|| fbuild_core::FbuildError::Other(format!("'{}' is out of range", choice)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- ide_state ----------

    #[test]
    fn ide_state_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        write_persisted_env(tmp.path(), "esp32dev").unwrap();
        assert_eq!(read_persisted_env(tmp.path()), Some("esp32dev".to_string()));
    }

    #[test]
    fn ide_state_absent_file_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_persisted_env(tmp.path()), None);
    }

    #[test]
    fn ide_state_malformed_json_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".fbuild")).unwrap();
        std::fs::write(ide_state_path(tmp.path()), "{ not json").unwrap();
        assert_eq!(read_persisted_env(tmp.path()), None);
    }

    #[test]
    fn ide_state_empty_file_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".fbuild")).unwrap();
        std::fs::write(ide_state_path(tmp.path()), "").unwrap();
        assert_eq!(read_persisted_env(tmp.path()), None);
    }

    // ---------- env resolution precedence ----------

    #[test]
    fn resolve_ide_env_explicit_wins_over_persisted() {
        let tmp = tempfile::tempdir().unwrap();
        write_persisted_env(tmp.path(), "persisted-env").unwrap();
        let resolved = resolve_ide_env(tmp.path(), Some("explicit-env".to_string())).unwrap();
        assert_eq!(resolved, "explicit-env");
    }

    #[test]
    fn resolve_ide_env_persisted_wins_over_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:default_env]\nplatform = espressif32\n[env:other]\nplatform = atmelavr\n",
        )
        .unwrap();
        write_persisted_env(tmp.path(), "other").unwrap();
        let resolved = resolve_ide_env(tmp.path(), None).unwrap();
        assert_eq!(resolved, "other");
    }

    #[test]
    fn resolve_ide_env_falls_back_to_platformio_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:only_env]\nplatform = espressif32\n",
        )
        .unwrap();
        let resolved = resolve_ide_env(tmp.path(), None).unwrap();
        assert_eq!(resolved, "only_env");
    }

    #[test]
    fn resolve_ide_env_errors_without_platformio_ini() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_ide_env(tmp.path(), None).is_err());
    }

    // ---------- mcu resolution + debug config (FastLED/fbuild#1076 Phase 3) ----------

    #[test]
    fn resolve_mcu_for_env_finds_rp2040_for_rpipico_board() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:pico]\nplatform = raspberrypi\nboard = rpipico\n",
        )
        .unwrap();
        let mcu = resolve_mcu_for_env(tmp.path(), "pico").unwrap();
        assert_eq!(mcu.as_deref(), Some("rp2040"));
    }

    #[test]
    fn resolve_mcu_for_env_finds_atmega328p_for_uno_board() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\n",
        )
        .unwrap();
        let mcu = resolve_mcu_for_env(tmp.path(), "uno").unwrap();
        assert_eq!(mcu.as_deref(), Some("atmega328p"));
    }

    #[test]
    fn resolve_mcu_for_env_none_for_missing_env() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\n",
        )
        .unwrap();
        let mcu = resolve_mcu_for_env(tmp.path(), "not_an_env").unwrap();
        assert_eq!(mcu, None);
    }

    #[test]
    fn regenerate_debug_config_emits_debug_json_for_rp2040() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:pico]\nplatform = raspberrypi\nboard = rpipico\n",
        )
        .unwrap();
        let (chip, debug_path) = regenerate_debug_config(tmp.path(), "pico").unwrap();
        assert_eq!(chip.as_deref(), Some("RP2040"));
        let debug_path = debug_path.expect("debug.json should be written");
        assert!(debug_path.exists());
        let content = std::fs::read_to_string(&debug_path).unwrap();
        assert!(content.contains("probe-rs"));
        assert!(content.contains("50101"));
    }

    #[test]
    fn regenerate_debug_config_no_debug_json_for_avr() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\n",
        )
        .unwrap();
        let (chip, debug_path) = regenerate_debug_config(tmp.path(), "uno").unwrap();
        assert_eq!(chip, None);
        assert_eq!(debug_path, None);
        assert!(!tmp.path().join(".zed/debug.json").exists());
    }

    // ---------- tasks.json merge ----------

    #[test]
    fn build_fbuild_tasks_pins_environment_in_args() {
        let tasks = build_fbuild_tasks("esp32dev", None);
        assert_eq!(tasks.len(), 11);
        for label in [
            "fbuild: Build",
            "fbuild: Build (clean)",
            "fbuild: Deploy",
            "fbuild: Deploy + Monitor",
            "fbuild: Monitor",
            "fbuild: Reset",
            "fbuild: Serial Plotter",
            "fbuild: Build Progress",
            "fbuild: Board Manager",
            "fbuild: Library Manager",
            "fbuild: Select environment",
        ] {
            assert!(
                tasks.iter().any(|t| t.label == label),
                "missing task {label}"
            );
        }
        let build = tasks.iter().find(|t| t.label == "fbuild: Build").unwrap();
        assert_eq!(build.args, vec!["build", "-e", "esp32dev"]);
        let select = tasks
            .iter()
            .find(|t| t.label == "fbuild: Select environment")
            .unwrap();
        assert_eq!(select.args, vec!["ide", "select"]);
        let plotter = tasks
            .iter()
            .find(|t| t.label == "fbuild: Serial Plotter")
            .unwrap();
        assert_eq!(plotter.args, vec!["plotter"]);
        let build_progress = tasks
            .iter()
            .find(|t| t.label == "fbuild: Build Progress")
            .unwrap();
        assert_eq!(build_progress.args, vec!["build-progress"]);
        let board_manager = tasks
            .iter()
            .find(|t| t.label == "fbuild: Board Manager")
            .unwrap();
        assert_eq!(board_manager.args, vec!["boards"]);
        let library_manager = tasks
            .iter()
            .find(|t| t.label == "fbuild: Library Manager")
            .unwrap();
        assert_eq!(library_manager.args, vec!["libraries", "-e", "esp32dev"]);
        // No debug-chip resolved -> no debug-server task.
        assert!(!tasks.iter().any(|t| t.label.contains("Debug server")));
    }

    #[test]
    fn build_fbuild_tasks_adds_debug_server_task_when_chip_resolved() {
        let tasks = build_fbuild_tasks("rpipico", Some("RP2040"));
        assert_eq!(tasks.len(), 12);
        let debug = tasks
            .iter()
            .find(|t| t.label == "fbuild: Debug server (probe-rs)")
            .unwrap();
        assert_eq!(debug.command, "probe-rs");
        assert_eq!(
            debug.args,
            vec!["dap-server", "--port", "50101", "--chip", "RP2040"]
        );
    }

    #[test]
    fn merge_tasks_preserves_user_tasks_and_replaces_fbuild_owned() {
        let existing = vec![
            ZedTask {
                label: "My custom task".to_string(),
                command: "echo".to_string(),
                args: vec!["hi".to_string()],
            },
            ZedTask {
                label: "fbuild: Build".to_string(),
                command: "fbuild".to_string(),
                args: vec!["build".to_string(), "-e".to_string(), "stale".to_string()],
            },
        ];
        let fresh = build_fbuild_tasks("esp32dev", None);
        let merged = merge_tasks(&existing, &fresh);

        assert!(merged.iter().any(|t| t.label == "My custom task"));
        let build = merged.iter().find(|t| t.label == "fbuild: Build").unwrap();
        assert_eq!(build.args, vec!["build", "-e", "esp32dev"]);
        // Stale fbuild-owned task replaced, not duplicated.
        assert_eq!(
            merged.iter().filter(|t| t.label == "fbuild: Build").count(),
            1
        );
    }

    #[test]
    fn merge_tasks_is_idempotent() {
        let fresh = build_fbuild_tasks("esp32dev", None);
        let once = merge_tasks(&[], &fresh);
        let twice = merge_tasks(&once, &fresh);
        assert_eq!(once, twice);
    }

    #[test]
    fn emit_zed_tasks_preserves_user_task_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let zed_dir = tmp.path().join(".zed");
        std::fs::create_dir_all(&zed_dir).unwrap();
        std::fs::write(
            zed_dir.join("tasks.json"),
            r#"[{"label": "My custom task", "command": "echo", "args": ["hi"]}]"#,
        )
        .unwrap();

        let path = emit_zed_tasks(tmp.path(), "esp32dev", None).unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        let tasks: Vec<ZedTask> = serde_json::from_str(&content).unwrap();
        assert!(tasks.iter().any(|t| t.label == "My custom task"));
        assert!(tasks.iter().any(|t| t.label == "fbuild: Build"));
    }

    // ---------- zed executable discovery (pure candidate list only) ----------

    #[test]
    fn known_zed_install_candidates_nonempty_when_env_vars_present() {
        // Just assert the function runs and returns platform-appropriate
        // shapes without touching the filesystem — actual existence checks
        // happen in `find_zed_executable`, which we deliberately don't
        // test here (must not launch/require zed in CI).
        let candidates = known_zed_install_candidates();
        if cfg!(windows) {
            assert!(
                candidates
                    .iter()
                    .all(|p| p.to_string_lossy().ends_with("zed.exe"))
            );
        } else {
            assert!(
                candidates
                    .iter()
                    .all(|p| p.to_string_lossy().contains("zed"))
            );
        }
    }

    #[test]
    fn zed_exe_name_matches_platform() {
        let name = zed_exe_name();
        if cfg!(windows) {
            assert_eq!(name, "zed.exe");
        } else {
            assert_eq!(name, "zed");
        }
    }
}
