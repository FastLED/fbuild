//! `fbuild debug`: no-probe GDB debugging orchestration
//! (FastLED/fbuild#1144).
//!
//! ## Scope of this PR
//!
//! FastLED/fbuild#1144 proposes a capability matrix across every platform
//! fbuild supports:
//!
//! | Target | Mechanism | Status |
//! |---|---|---|
//! | ESP32 family | orchestrate the existing native IDF/ROM gdbstub | **implemented here** |
//! | CH32V (RISC-V) | injected stub (EBREAK + trap handler) | planned — depends on FastLED/soundwave#38 |
//! | AVR | none — no trap architecture | honestly unsupported, documented |
//! | ARM (Teensy/STM/nRF/RP2040) | DebugMon/BKPT stubs, per-family | out of scope for this PR |
//!
//! This module ships the capability matrix (as code, see
//! [`DebugSupport`]/[`debug_support_for_platform`]) for all of the above so
//! the CLI always gives a first-class, honest answer instead of a cryptic
//! failure — but the actual attach *orchestration* (build → flash → find
//! the port → launch gdb) is implemented for ESP32 only. CH32V and AVR
//! print a clear explanatory message and exit nonzero-but-clean, with no
//! attempt at orchestration.
//!
//! ## What's real and what isn't for ESP32
//!
//! The IDF/ROM gdbstub in Arduino-ESP32 cores is a stock component of the
//! framework — it doesn't need fbuild to inject anything to *exist*. What
//! it needs to actually be reached over serial is
//! `CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=y` (see
//! `fbuild_config::sdkconfig::SdkConfigSummary`), which is off by default
//! (Arduino-ESP32's stock `sdkconfig.h` ships `panic=print`, i.e. a plain
//! backtrace on panic, not an attachable gdbstub). `docs/sdkconfig.md`
//! documents fbuild's sdkconfig-override story as a **design proposal, not
//! yet implemented** — there is no sanctioned knob today (beyond a raw
//! `build_flags = -D CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=1` in
//! `platformio.ini`, which is the *existing* general-purpose build-flags
//! extension point, not something this command adds).
//!
//! This command therefore does **not** silently assume gdbstub-on-panic is
//! enabled. [`gdbstub_readiness_note`] inspects the project's
//! `SdkConfigSummary` and always tells the user, in the command's output,
//! whether the *current* build is actually gdbstub-reachable-on-panic or
//! only has the framework default (`panic=print`) — so a user who hits
//! "gdb attached but the target isn't listening" understands why, instead
//! of it looking like an fbuild bug. The command still proceeds: on ESP32,
//! `target remote` also works against a *live* target break (Ctrl+C from
//! gdb, or a deliberate `abort()`/breakpoint) even without
//! `CONFIG_ESP_SYSTEM_PANIC_GDBSTUB`, since the ROM/IDF gdbstub component
//! itself is always present in the framework — only the *panic-triggers-it*
//! behavior depends on the sdkconfig knob.
//!
//! ## Orchestration
//!
//! 1. Resolve `project_dir` + `-e/--environment` to a `Platform`/mcu via
//!    the same `platformio.ini` → `BoardConfig` path `cli::ide::resolve_mcu_for_env`
//!    and `cli::deploy::infer_cli_default_emulator_kind` already use.
//! 2. Unless `--no-flash`, build + flash through the existing daemon
//!    deploy path (`cli::deploy::run_deploy`) — the same code path
//!    `fbuild deploy` uses, so there is no parallel build/flash
//!    implementation to keep in sync.
//! 3. Locate the built ELF via `fbuild_build::symbol_analyzer::discover_elf_in_project`
//!    — the same resolution `fbuild symbols` uses (`build_info.json` →
//!    `.fbuild/build/**/firmware.elf` → `.pio/build/**/firmware.elf` →
//!    loose `*.elf`).
//! 4. Find the CDC/serial port: `--port` wins; otherwise ask the daemon
//!    for the device list and use the port if there's exactly one
//!    candidate, else ask the user to disambiguate with `--port`.
//! 5. Resolve a `gdb` binary for the MCU's architecture: prefer deriving
//!    it from the build's own `gcc` path (`build_info.json`'s `cc_path`,
//!    GCC cross-toolchain naming convention: swap the `gcc` suffix for
//!    `gdb`), else search `PATH` for the architecture's conventional name
//!    (`xtensa-esp32-elf-gdb` / `riscv32-esp-elf-gdb` and friends). Neither
//!    `fbuild-toolchain`'s `Esp32Toolchain` nor its `Toolchain` trait
//!    resolve `gdb` today (only gcc/g++/ar/objcopy/size) — this module
//!    adds the extra resolution step rather than modifying that trait,
//!    since the toolchain crate has no notion of "just find gdb for this
//!    already-installed toolchain" without the download URL/checksum
//!    plumbing `Esp32Toolchain::from_resolved` requires.
//! 6. Spawn `gdb` interactively (inherits stdio — the default for
//!    `std::process::Command` when stdio isn't otherwise configured) with
//!    `-ex "set serial baud <baud>" -ex "target remote <port>" <elf>`.

use std::path::{Path, PathBuf};
use std::process::Command;

use fbuild_build::build_info::{find_build_info_near, load_build_info};
use fbuild_build::symbol_analyzer::discover_elf_in_project;
use fbuild_config::sdkconfig::SdkConfigSummary;
use fbuild_core::{FbuildError, Platform, Result};

use crate::daemon_client::{self, DaemonClient};
use crate::output;

/// FastLED/fbuild#1144 tracking issue, quoted in every "not supported yet"
/// message so users land on the right context instead of guessing.
const ISSUE_URL: &str = "https://github.com/FastLED/fbuild/issues/1144";

/// Default upload/gdbstub baud rate. Matches the IDF/ROM gdbstub's fixed
/// serial rate on stock Arduino-ESP32 boards; `platformio.ini`'s
/// `monitor_speed`/`upload_speed` aren't consulted here because the
/// gdbstub protocol runs at a fixed rate independent of the app's own
/// UART usage.
const DEFAULT_GDBSTUB_BAUD: u32 = 115_200;

// ---------------------------------------------------------------------
// Capability matrix (FastLED/fbuild#1144)
// ---------------------------------------------------------------------

/// How (if at all) `fbuild debug` can attach a GDB session for a given
/// platform. Pure data — no I/O, directly testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DebugSupport {
    /// The platform ships its own on-device gdbstub; fbuild only needs to
    /// orchestrate build/flash/port-discovery/attach. ESP32 family today.
    NativeGdbStub,
    /// A stub could be injected (trap handler + transport) but isn't
    /// implemented yet. CH32V — tracked jointly with FastLED/soundwave#38.
    Injected,
    /// No trap architecture exists to host a stub at all. AVR.
    Unsupported,
}

/// The capability-matrix lookup: fbuild's `Platform` enum → [`DebugSupport`].
/// Conservative on purpose — any platform not explicitly evaluated (ARM
/// families: Teensy/STM32/nRF/RP2040, per the issue's "evaluate
/// individually, later" row) is treated as [`DebugSupport::Unsupported`]
/// rather than silently claiming a capability fbuild hasn't verified.
pub(crate) fn debug_support_for_platform(platform: Platform) -> DebugSupport {
    match platform {
        Platform::Espressif32 => DebugSupport::NativeGdbStub,
        Platform::Ch32v => DebugSupport::Injected,
        _ => DebugSupport::Unsupported,
    }
}

/// The first-class explanatory message printed (and returned as the
/// process's clean nonzero-exit error) for anything that isn't
/// [`DebugSupport::NativeGdbStub`]. Pure so it's directly testable without
/// stdout capture.
pub(crate) fn debug_support_message(
    platform: Platform,
    mcu: &str,
    support: DebugSupport,
) -> String {
    match support {
        DebugSupport::NativeGdbStub => format!(
            "fbuild debug: {mcu} has a native gdbstub — this message should not be reachable, please file a bug ({ISSUE_URL})"
        ),
        DebugSupport::Injected => format!(
            "fbuild debug: {mcu} ({platform:?}) debugging is planned but not yet implemented. \
             An injected GDB stub (EBREAK + trap handler over CDC) is the proposed mechanism \
             for CH32V, tracked jointly with the on-device side in FastLED/soundwave#38. \
             See {ISSUE_URL}."
        ),
        DebugSupport::Unsupported => {
            if matches!(platform, Platform::AtmelAvr | Platform::AtmelMegaAvr) {
                format!(
                    "fbuild debug: {mcu} (AVR) cannot host a GDB stub — AVR has no trap \
                     architecture to catch a breakpoint/exception and hand control to a \
                     debug monitor. This is a hardware limitation, not a missing fbuild \
                     feature; it is intentionally unsupported. See {ISSUE_URL}."
                )
            } else {
                format!(
                    "fbuild debug: {mcu} ({platform:?}) isn't supported yet. This release \
                     of `fbuild debug` only orchestrates the ESP32 family's native gdbstub; \
                     other targets (ARM Cortex-M families, RP2040) are evaluated individually \
                     in a follow-up. See {ISSUE_URL}."
                )
            }
        }
    }
}

// ---------------------------------------------------------------------
// gdbstub-readiness note (sdkconfig honesty, see module docs)
// ---------------------------------------------------------------------

/// Describe, honestly, whether the project's current sdkconfig makes the
/// ESP32 gdbstub reachable on panic. Pure over an already-loaded
/// [`SdkConfigSummary`] so it's directly testable.
pub(crate) fn gdbstub_readiness_note(summary: &SdkConfigSummary) -> String {
    if summary.panic_gdbstub {
        "gdbstub-on-panic: ENABLED (CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=y found in sdkconfig) — \
         a panic will drop straight into the attached gdb session."
            .to_string()
    } else {
        "gdbstub-on-panic: NOT enabled (framework default is panic=print — a panic prints a \
         backtrace and reboots rather than waiting for gdb). The ROM/IDF gdbstub is still \
         present and reachable for a live break (Ctrl+C in gdb, or a deliberate breakpoint); \
         to make panics themselves stop in gdb, add \
         `build_flags = -D CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=1` to this environment in \
         platformio.ini (fbuild's own sdkconfig-override layer is a design proposal, not yet \
         implemented — see docs/sdkconfig.md)."
            .to_string()
    }
}

// ---------------------------------------------------------------------
// gdb binary resolution
// ---------------------------------------------------------------------

/// Derive a `gdb` path from a `gcc` path using the GCC cross-toolchain
/// naming convention (`<prefix>gcc` → `<prefix>gdb`, same directory, same
/// extension). Mirrors `fbuild_build::symbol_analyzer::derive_cppfilt_path`'s
/// `nm` → `c++filt` derivation. Pure — doesn't check the result exists.
pub(crate) fn derive_gdb_path(gcc_path: &Path) -> PathBuf {
    let parent = gcc_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = gcc_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = gcc_path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let gdb_stem = if let Some(prefix) = stem.strip_suffix("gcc") {
        format!("{prefix}gdb")
    } else {
        format!("{stem}-gdb")
    };
    if ext.is_empty() {
        parent.join(gdb_stem)
    } else {
        parent.join(format!("{gdb_stem}.{ext}"))
    }
}

/// Candidate `gdb` binary names to search `PATH` for, given the ESP32
/// toolchain prefix (`xtensa-esp32-elf-`, `xtensa-esp32s3-elf-`,
/// `riscv32-esp-elf-`, ...). Includes both the MCU-specific prefix
/// convention `fbuild_build::esp32::mcu_config` uses and the generic
/// `xtensa-esp32-elf-gdb` name older Arduino-ESP32 toolchain releases
/// shipped, since a project might have either on `PATH`. Pure — returns
/// bare names, extension-less; callers add the platform executable suffix.
pub(crate) fn gdb_candidate_names(toolchain_prefix: &str) -> Vec<String> {
    let mut names = vec![format!("{toolchain_prefix}gdb")];
    if toolchain_prefix.starts_with("riscv32") {
        if !names.contains(&"riscv32-esp-elf-gdb".to_string()) {
            names.push("riscv32-esp-elf-gdb".to_string());
        }
    } else if !names.contains(&"xtensa-esp32-elf-gdb".to_string()) {
        names.push("xtensa-esp32-elf-gdb".to_string());
    }
    names
}

fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Search a list of directories (in order) for the first existing
/// candidate binary name. Pure function of its inputs — I/O-free beyond
/// `Path::exists`, so tests can point it at a `tempdir` fixture instead of
/// the real `PATH`/toolchain cache.
pub(crate) fn find_gdb_in_dirs(dirs: &[PathBuf], candidate_names: &[String]) -> Option<PathBuf> {
    for dir in dirs {
        for name in candidate_names {
            let candidate = dir.join(exe_name(name));
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

/// Resolve a `gdb` binary for the given MCU. Prefers deriving it from the
/// build's own `gcc` (via `build_info.json`, found by walking up from the
/// ELF's directory — same discovery `fbuild symbols` uses), then falls
/// back to `PATH`. Returns a clear install-guidance error when neither
/// resolves, naming the toolchain prefix so the user knows what to
/// install/add to `PATH`.
fn resolve_gdb_path(elf_path: &Path, toolchain_prefix: &str) -> Result<PathBuf> {
    if let Some(build_info_path) = elf_path.parent().and_then(find_build_info_near) {
        if let Ok((_env, info)) = load_build_info(&build_info_path) {
            let cc_path = info.cc_path.as_path();
            if !cc_path.as_os_str().is_empty() {
                let candidate = derive_gdb_path(cc_path);
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        }
    }

    let candidate_names = gdb_candidate_names(toolchain_prefix);
    if let Some(found) = find_gdb_in_dirs(&path_dirs(), &candidate_names) {
        return Ok(found);
    }

    Err(FbuildError::Other(format!(
        "gdb not found: looked for {} derived from this build's gcc, then searched PATH for {}.\n\
         Install the ESP32 GCC toolchain (it ships gdb alongside gcc) and ensure its bin/ \
         directory is on PATH, or point `fbuild build` at a toolchain whose bin/ contains gdb.",
        candidate_names.first().cloned().unwrap_or_default(),
        candidate_names.join(", "),
    )))
}

// ---------------------------------------------------------------------
// gdb argv construction
// ---------------------------------------------------------------------

/// Windows serial device names below COM10 work bare; `\\.\COMx` is the
/// documented escape hatch for COM10+ and is accepted for any COM name.
/// fbuild-serial doesn't have an existing helper for this (it hands bare
/// `"COM3"`-style names to the `serialport` crate, which does its own
/// translation internally) — gdb's `target remote` talks directly to the
/// OS device, so this module does the translation itself. No-op on other
/// platforms and for names that already use the `\\.\` prefix.
pub(crate) fn windows_gdb_serial_target(port: &str) -> String {
    if cfg!(windows) && !port.starts_with(r"\\.\") && !port.contains('/') {
        format!(r"\\.\{port}")
    } else {
        port.to_string()
    }
}

/// Build the gdb argv for attaching to the IDF/ROM serial gdbstub. Pure —
/// directly testable. Order matters: baud must be set before `target
/// remote` opens the serial line.
pub(crate) fn build_gdb_argv(elf_path: &Path, serial_target: &str, baud: u32) -> Vec<String> {
    vec![
        "-ex".to_string(),
        format!("set serial baud {baud}"),
        "-ex".to_string(),
        format!("target remote {serial_target}"),
        elf_path.display().to_string(),
    ]
}

// ---------------------------------------------------------------------
// ELF resolution
// ---------------------------------------------------------------------

/// Resolve the ELF to debug: same discovery `fbuild symbols` uses
/// (`build_info.json` → `.fbuild/build/**/firmware.elf` →
/// `.pio/build/**/firmware.elf` → loose `*.elf`). Returns a clear error
/// naming `--no-flash` and `fbuild build` as the two ways to get an ELF in
/// place.
fn resolve_debug_elf(project_dir: &Path) -> Result<PathBuf> {
    discover_elf_in_project(project_dir).ok_or_else(|| {
        FbuildError::Other(format!(
            "no ELF found under {} — run `fbuild debug` without --no-flash to build one, \
             or run `fbuild build` first",
            project_dir.display()
        ))
    })
}

// ---------------------------------------------------------------------
// Env/platform/mcu resolution (mirrors cli::ide::resolve_mcu_for_env /
// cli::deploy::infer_cli_default_emulator_kind)
// ---------------------------------------------------------------------

struct ResolvedTarget {
    env_name: String,
    platform: Platform,
    mcu: String,
}

fn resolve_debug_target(project_dir: &Path, environment: Option<&str>) -> Result<ResolvedTarget> {
    let config = fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))
        .map_err(|e| FbuildError::Other(format!("failed to parse platformio.ini: {e}")))?;
    let env_name = environment
        .map(|s| s.to_string())
        .or_else(|| config.get_default_environment().map(|s| s.to_string()))
        .ok_or_else(|| {
            FbuildError::Other(
                "no environment specified and platformio.ini has no default_envs".to_string(),
            )
        })?;
    let env_config = config
        .get_env_config(&env_name)
        .map_err(|e| FbuildError::Other(format!("invalid environment '{env_name}': {e}")))?;
    let platform_str = env_config.get("platform").cloned().unwrap_or_default();
    let platform = Platform::from_platform_str(&platform_str).ok_or_else(|| {
        FbuildError::Other(format!(
            "environment '{env_name}' has unrecognized platform '{platform_str}'"
        ))
    })?;
    let board_id = env_config
        .get("board")
        .cloned()
        .ok_or_else(|| FbuildError::Other(format!("environment '{env_name}' has no 'board ='")))?;
    let board_overrides = config.get_board_overrides(&env_name).unwrap_or_default();
    let board = fbuild_config::BoardConfig::from_board_id_with_override_fallback(
        &board_id,
        &board_overrides,
        Some(project_dir),
    )
    .ok_or_else(|| FbuildError::Other(format!("unknown board '{board_id}'")))?;

    Ok(ResolvedTarget {
        env_name,
        platform,
        mcu: board.mcu,
    })
}

// ---------------------------------------------------------------------
// Port discovery
// ---------------------------------------------------------------------

async fn resolve_debug_port(client: &DaemonClient, explicit: Option<String>) -> Result<String> {
    if let Some(port) = explicit {
        return Ok(port);
    }
    let devices = client.list_devices(true).await?.devices;
    let cdc_ports: Vec<&str> = devices
        .iter()
        .filter(|d| d.is_cdc != Some(false))
        .map(|d| d.port.as_str())
        .collect();
    match cdc_ports.as_slice() {
        [] => Err(FbuildError::Other(
            "no serial devices found — connect the board and retry, or pass --port explicitly"
                .to_string(),
        )),
        [single] => Ok((*single).to_string()),
        multiple => Err(FbuildError::Other(format!(
            "multiple serial devices found ({}) — pass --port to pick one",
            multiple.join(", ")
        ))),
    }
}

// ---------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------

/// `fbuild debug [project_dir] [-e env] [--no-flash] [--port <serial>]`
///
/// The interactive `gdb` spawn at the end of this function is
/// deliberately untested: it inherits the parent's stdio and hands
/// control to an external interactive program, so there is nothing to
/// assert against short of a real toolchain + real hardware (or a fake
/// `gdb` shim, which would only be testing that `Command::status()` works
/// — not this module's logic). Every step *up to* the spawn (capability
/// lookup, target resolution, ELF resolution, gdb resolution, argv
/// construction, port discovery) is pure or covered by unit tests below.
pub async fn run_debug(
    project_dir: String,
    environment: Option<String>,
    no_flash: bool,
    port: Option<String>,
) -> Result<()> {
    let project_path = PathBuf::from(&project_dir);
    let target = resolve_debug_target(&project_path, environment.as_deref())?;

    let support = debug_support_for_platform(target.platform);
    if support != DebugSupport::NativeGdbStub {
        let message = debug_support_message(target.platform, &target.mcu, support);
        output::error(&message);
        return Err(FbuildError::CommandFailed {
            message,
            exit_code: 1,
        });
    }

    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

    let sdk_summary = SdkConfigSummary::from_project_dir(&project_path);
    output::diagnostic(gdbstub_readiness_note(&sdk_summary));

    if no_flash {
        output::progress("--no-flash: using the existing build without rebuilding/reflashing");
    } else {
        output::progress(format!(
            "Building and flashing '{}' (env={}) before attaching gdb...",
            project_dir, target.env_name
        ));
        super::deploy::run_deploy(
            project_dir.clone(),
            Some(target.env_name.clone()),
            port.clone(),
            None,
            None,
            false,
            false,
            false,
            false,
            None,
            None,
            None,
            None,
            false,
            false,
            false,
            30,
            None,
            false,
            None,
            None,
            None,
            None,
            fbuild_core::usb::UsbRecoveryPolicy::Default,
        )
        .await?;
    }

    let elf_path = resolve_debug_elf(&project_path)?;
    output::diagnostic(format!("Using ELF: {}", elf_path.display()));

    let serial_port = resolve_debug_port(&client, port).await?;
    let serial_target = windows_gdb_serial_target(&serial_port);

    let mcu_config = fbuild_build::esp32::mcu_config::get_mcu_config(&target.mcu)
        .map_err(|e| FbuildError::Other(format!("unrecognized ESP32 mcu '{}': {e}", target.mcu)))?;
    let gdb_path = resolve_gdb_path(&elf_path, &mcu_config.toolchain_prefix())?;
    output::diagnostic(format!("Using gdb: {}", gdb_path.display()));

    let argv = build_gdb_argv(&elf_path, &serial_target, DEFAULT_GDBSTUB_BAUD);
    output::progress(format!(
        "Launching: {} {}",
        gdb_path.display(),
        argv.join(" ")
    ));

    // Inherits stdio by default (std::process::Command's default when
    // stdin/stdout/stderr aren't explicitly redirected) — gdb runs
    // interactively in the user's terminal exactly as if they'd typed the
    // command themselves.
    let status = Command::new(&gdb_path)
        .args(&argv)
        .status()
        .map_err(|e| FbuildError::Other(format!("failed to launch {}: {e}", gdb_path.display())))?;

    if !status.success() {
        return Err(FbuildError::CommandFailed {
            message: format!("gdb exited with {status}"),
            exit_code: status.code().unwrap_or(1),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- capability matrix ----------

    #[test]
    fn esp32_family_gets_native_gdbstub() {
        assert_eq!(
            debug_support_for_platform(Platform::Espressif32),
            DebugSupport::NativeGdbStub
        );
    }

    #[test]
    fn ch32v_gets_injected_planned() {
        assert_eq!(
            debug_support_for_platform(Platform::Ch32v),
            DebugSupport::Injected
        );
    }

    #[test]
    fn avr_family_gets_unsupported() {
        assert_eq!(
            debug_support_for_platform(Platform::AtmelAvr),
            DebugSupport::Unsupported
        );
        assert_eq!(
            debug_support_for_platform(Platform::AtmelMegaAvr),
            DebugSupport::Unsupported
        );
    }

    #[test]
    fn unevaluated_platforms_default_unsupported() {
        for p in [
            Platform::Ststm32,
            Platform::Teensy,
            Platform::NordicNrf52,
            Platform::RaspberryPi,
        ] {
            assert_eq!(debug_support_for_platform(p), DebugSupport::Unsupported);
        }
    }

    #[test]
    fn ch32v_message_mentions_planned_and_issue_link_and_soundwave() {
        let msg = debug_support_message(Platform::Ch32v, "ch32v203c8t6", DebugSupport::Injected);
        assert!(msg.contains("planned"));
        assert!(msg.contains("ch32v203c8t6"));
        assert!(msg.contains("soundwave#38"));
        assert!(msg.contains(ISSUE_URL));
    }

    #[test]
    fn avr_message_explains_no_trap_architecture() {
        let msg =
            debug_support_message(Platform::AtmelAvr, "atmega328p", DebugSupport::Unsupported);
        assert!(msg.contains("atmega328p"));
        assert!(msg.contains("no trap architecture"));
        assert!(msg.contains(ISSUE_URL));
    }

    #[test]
    fn generic_unsupported_message_names_target() {
        let msg =
            debug_support_message(Platform::Ststm32, "stm32f103c8", DebugSupport::Unsupported);
        assert!(msg.contains("stm32f103c8"));
        assert!(msg.contains(ISSUE_URL));
    }

    // ---------- gdbstub readiness note ----------

    #[test]
    fn readiness_note_reports_enabled() {
        let summary = SdkConfigSummary {
            panic_gdbstub: true,
            ..Default::default()
        };
        let note = gdbstub_readiness_note(&summary);
        assert!(note.contains("ENABLED"));
    }

    #[test]
    fn readiness_note_reports_disabled_with_guidance() {
        let summary = SdkConfigSummary::default();
        let note = gdbstub_readiness_note(&summary);
        assert!(note.contains("NOT enabled"));
        assert!(note.contains("CONFIG_ESP_SYSTEM_PANIC_GDBSTUB"));
        assert!(note.contains("docs/sdkconfig.md"));
    }

    // ---------- gdb path derivation ----------

    #[test]
    fn derive_gdb_path_swaps_gcc_suffix() {
        let gcc = Path::new("/opt/toolchain/bin/xtensa-esp32-elf-gcc");
        assert_eq!(
            derive_gdb_path(gcc),
            PathBuf::from("/opt/toolchain/bin/xtensa-esp32-elf-gdb")
        );
    }

    #[test]
    fn derive_gdb_path_preserves_windows_exe_extension() {
        let gcc = Path::new(r"C:\toolchain\bin\riscv32-esp-elf-gcc.exe");
        assert_eq!(
            derive_gdb_path(gcc),
            PathBuf::from(r"C:\toolchain\bin\riscv32-esp-elf-gdb.exe")
        );
    }

    #[test]
    fn gdb_candidate_names_riscv() {
        let names = gdb_candidate_names("riscv32-esp-elf-");
        assert!(names.contains(&"riscv32-esp-elf-gdb".to_string()));
    }

    #[test]
    fn gdb_candidate_names_xtensa_includes_generic_fallback() {
        let names = gdb_candidate_names("xtensa-esp32s3-elf-");
        assert!(names.contains(&"xtensa-esp32s3-elf-gdb".to_string()));
        assert!(names.contains(&"xtensa-esp32-elf-gdb".to_string()));
    }

    // ---------- gdb binary resolution over a fake toolchain dir ----------

    #[test]
    fn find_gdb_in_dirs_finds_first_match() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("toolchain").join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let gdb_name = exe_name("xtensa-esp32-elf-gdb");
        std::fs::write(bin_dir.join(&gdb_name), b"").unwrap();

        let found = find_gdb_in_dirs(
            std::slice::from_ref(&bin_dir),
            &gdb_candidate_names("xtensa-esp32-elf-"),
        );
        assert_eq!(found, Some(bin_dir.join(gdb_name)));
    }

    #[test]
    fn find_gdb_in_dirs_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("empty-bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let found = find_gdb_in_dirs(&[bin_dir], &gdb_candidate_names("riscv32-esp-elf-"));
        assert_eq!(found, None);
    }

    #[test]
    fn find_gdb_in_dirs_searches_dirs_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let dir_a = tmp.path().join("a");
        let dir_b = tmp.path().join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        let names = gdb_candidate_names("riscv32-esp-elf-");
        std::fs::write(dir_b.join(exe_name(&names[0])), b"").unwrap();

        let found = find_gdb_in_dirs(&[dir_a.clone(), dir_b.clone()], &names);
        assert_eq!(found, Some(dir_b.join(exe_name(&names[0]))));
    }

    // ---------- gdb argv construction ----------

    #[test]
    fn build_gdb_argv_orders_baud_before_target_remote() {
        let elf = Path::new("/proj/.fbuild/build/esp32dev/release/firmware.elf");
        let argv = build_gdb_argv(elf, "COM5", 115_200);
        assert_eq!(
            argv,
            vec![
                "-ex".to_string(),
                "set serial baud 115200".to_string(),
                "-ex".to_string(),
                "target remote COM5".to_string(),
                "/proj/.fbuild/build/esp32dev/release/firmware.elf".to_string(),
            ]
        );
    }

    #[test]
    fn windows_gdb_serial_target_leaves_unix_paths_alone() {
        assert_eq!(windows_gdb_serial_target("/dev/ttyUSB0"), "/dev/ttyUSB0");
    }

    #[cfg(windows)]
    #[test]
    fn windows_gdb_serial_target_prefixes_bare_com_ports() {
        assert_eq!(windows_gdb_serial_target("COM5"), r"\\.\COM5");
    }

    #[cfg(windows)]
    #[test]
    fn windows_gdb_serial_target_is_idempotent() {
        assert_eq!(windows_gdb_serial_target(r"\\.\COM5"), r"\\.\COM5");
    }

    // ---------- ELF resolution against a tempdir fixture ----------

    #[test]
    fn resolve_debug_elf_finds_loose_elf_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("firmware.elf"), b"\x7fELF").unwrap();
        let resolved = resolve_debug_elf(tmp.path()).unwrap();
        assert_eq!(resolved, tmp.path().join("firmware.elf"));
    }

    #[test]
    fn resolve_debug_elf_errors_with_actionable_message_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_debug_elf(tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--no-flash") || msg.contains("fbuild build"));
    }

    // ---------- env/platform/mcu resolution ----------

    #[test]
    fn resolve_debug_target_reads_platform_and_mcu() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:esp32dev]\nplatform = espressif32\nboard = esp32dev\n",
        )
        .unwrap();
        let target = resolve_debug_target(tmp.path(), Some("esp32dev")).unwrap();
        assert_eq!(target.env_name, "esp32dev");
        assert_eq!(target.platform, Platform::Espressif32);
        assert_eq!(target.mcu, "esp32");
    }

    #[test]
    fn resolve_debug_target_errors_on_unknown_env() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:esp32dev]\nplatform = espressif32\nboard = esp32dev\n",
        )
        .unwrap();
        assert!(resolve_debug_target(tmp.path(), Some("nope")).is_err());
    }

    // ---------- CLI shape ----------
    // See crate-level `cli::tests` for `Cli::try_parse_from` coverage of
    // `Commands::Debug` (project_dir/environment/no_flash/port wiring).
}
