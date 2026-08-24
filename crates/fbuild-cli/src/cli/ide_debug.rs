//! `.zed/debug.json` generation for `fbuild ide` (FastLED/fbuild#1076 Phase
//! 3, milestone 1: probe-rs-supported targets only).
//!
//! Zed's debugger is DAP-native and supports attaching to an
//! already-running external DAP server via a `tcp_connection` entry in
//! `.zed/debug.json`. probe-rs ships exactly such a server
//! (`probe-rs dap-server`, the same one its VS Code extension drives), so
//! for chips probe-rs supports we can generate a working attach config with
//! zero Zed extension involvement.
//!
//! Milestone 1 is deliberately narrow: **ARM Cortex-M families + RP2040**
//! only. OpenOCD-based targets (ESP32 via openocd-esp32, AVR) are out of
//! scope — probe-rs doesn't support them, and driving OpenOCD would need a
//! DAP<->GDB bridge Zed has no equivalent of. For those targets we emit
//! nothing and surface a first-class "not supported yet" note instead of
//! silently doing nothing or failing.
//!
//! ## Chip mapping
//!
//! [`probe_rs_chip_for_mcu`] maps fbuild's `BoardConfig::mcu` string (e.g.
//! `"rp2040"`, `"stm32f103c8t6"`, as emitted by the board JSON database
//! under `crates/fbuild-config/assets/boards/json/`) to a probe-rs chip
//! identifier (the `--chip` argument to `probe-rs`/`probe-rs dap-server`).
//! The table is intentionally short: a wrong chip name silently produces a
//! debug session that can't find the target (or worse, targets the wrong
//! silicon), which is worse than no entry at all. Unmapped/unknown MCUs
//! return `None` rather than guessing.
//!
//! | fbuild `mcu` | probe-rs chip | confidence |
//! |---|---|---|
//! | `rp2040` | `RP2040` | high — probe-rs's flagship supported target |
//! | `rp2350` | `RP235x` | high — probe-rs's target file is `RP235x.yaml` and the ARM-core chip entry is named `RP235x` (verified against probe-rs/probe-rs master targets) |
//! | `imxrt1062` | `MIMXRT1060` | high — probe-rs ships `MIMXRT1060.yaml` whose single chip entry `MIMXRT1060` covers the 1061/1062 parts (Teensy 4.x). Note Teensy boards need the debug pads wired to a probe; the config is still correct when they are |
//! | `nrf52840` | `nRF52840_xxAA` | high — standard probe-rs-target naming for Nordic parts (package-suffixed SVD-derived name) |
//! | `stm32f103c8t6` | `STM32F103C8` | high — `STM32F1_Series.yaml` names chips by bare part number (`STM32F103C8`), verified against probe-rs master targets |
//!
//! Deliberately **not** mapped even though fbuild has board data for them:
//! any other STM32 family/package not in the table above, and anything AVR
//! or ESP32 (not probe-rs targets at all).

use std::path::Path;

use fbuild_core::path::NormalizedPath;
use serde::{Deserialize, Serialize};

/// Fixed default port `fbuild ide` tells `probe-rs dap-server` to listen on
/// and tells Zed to attach to. Documented in `docs/reference/cli.md`.
pub(crate) const PROBE_RS_DAP_PORT: u16 = 50101;

/// Label prefix that marks a Zed debug entry as fbuild-owned, mirroring
/// `ide::FBUILD_TASK_PREFIX` for `.zed/tasks.json`.
const FBUILD_DEBUG_PREFIX: &str = "fbuild: ";

/// Map an fbuild `BoardConfig::mcu` string to a probe-rs chip identifier.
/// Case-insensitive on the input (board JSON is lowercase today, but this
/// doesn't assume it stays that way). Returns `None` for anything not in
/// the conservative table documented on this module.
pub(crate) fn probe_rs_chip_for_mcu(mcu: &str) -> Option<&'static str> {
    match mcu.to_ascii_lowercase().as_str() {
        "rp2040" => Some("RP2040"),
        "rp2350" => Some("RP235x"),
        "imxrt1062" => Some("MIMXRT1060"),
        "nrf52840" => Some("nRF52840_xxAA"),
        "stm32f103c8t6" | "stm32f103c8" => Some("STM32F103C8"),
        _ => None,
    }
}

/// The concise, first-class "not supported yet" note printed during
/// `fbuild ide` for boards/MCUs milestone 1 doesn't cover. Pure so it's
/// directly testable without stdout capture.
pub(crate) fn unsupported_debug_note(board_or_mcu: &str) -> String {
    format!("debug config not supported for {board_or_mcu} (milestone 1 is probe-rs targets)")
}

// ---------------------------------------------------------------------
// .zed/debug.json — merge-don't-clobber
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ZedTcpConnection {
    host: String,
    port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ZedDebugEntry {
    label: String,
    adapter: String,
    request: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    program: Option<String>,
    tcp_connection: ZedTcpConnection,
}

/// Build the fbuild-owned debug entry. `env_name` isn't folded into the
/// label — like `.zed/tasks.json`'s "fbuild: Build" task, the label stays
/// unqualified by environment because milestone 1 only ever configures the
/// one currently-selected environment (`fbuild ide select`) at a time; the
/// entry's contents (chip via the running `probe-rs dap-server`, `program`)
/// are what actually change per environment. `elf_path`, when resolvable, is
/// passed through as `program` so probe-rs can load symbols; the path does
/// not need to exist yet at config-generation time (it's the expected
/// `fbuild build -e <env>` output location).
fn build_debug_entry(_env_name: &str, elf_path: Option<&Path>) -> ZedDebugEntry {
    ZedDebugEntry {
        label: format!("{FBUILD_DEBUG_PREFIX}Debug (probe-rs attach)"),
        adapter: "probe-rs".to_string(),
        request: "attach".to_string(),
        program: elf_path.map(|p| p.display().to_string()),
        tcp_connection: ZedTcpConnection {
            host: "127.0.0.1".to_string(),
            port: PROBE_RS_DAP_PORT,
        },
    }
}

/// Merge fbuild's debug entry into existing `.zed/debug.json` content: any
/// existing entry whose label starts with `"fbuild: "` is replaced; every
/// other (user) entry is preserved verbatim, in its original position.
fn merge_debug_entries(
    existing: &[ZedDebugEntry],
    fbuild_entries: &[ZedDebugEntry],
) -> Vec<ZedDebugEntry> {
    let mut merged: Vec<ZedDebugEntry> = existing
        .iter()
        .filter(|e| !e.label.starts_with(FBUILD_DEBUG_PREFIX))
        .cloned()
        .collect();
    merged.extend(fbuild_entries.iter().cloned());
    merged
}

fn read_debug_file(path: &Path) -> Vec<ZedDebugEntry> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    if content.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str(&content).unwrap_or_default()
}

/// Write `.zed/debug.json` with fbuild's probe-rs attach entry merged in,
/// preserving any user-authored entries. Only called when
/// [`probe_rs_chip_for_mcu`] resolved a chip for the current environment.
pub(crate) fn emit_zed_debug(
    project_path: &Path,
    env_name: &str,
    elf_path: Option<&Path>,
) -> fbuild_core::Result<NormalizedPath> {
    let zed_dir = project_path.join(".zed");
    std::fs::create_dir_all(&zed_dir).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to create {}: {}", zed_dir.display(), e))
    })?;
    let debug_path = NormalizedPath::from(zed_dir.join("debug.json"));
    let existing = read_debug_file(&debug_path);
    let fresh = vec![build_debug_entry(env_name, elf_path)];
    let merged = merge_debug_entries(&existing, &fresh);
    let mut json = serde_json::to_string_pretty(&merged).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to serialize debug.json: {}", e))
    })?;
    json.push('\n');
    std::fs::write(&debug_path, json).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to write {}: {}", debug_path.display(), e))
    })?;
    Ok(debug_path)
}

/// The expected ELF output path for `env_name`, best-effort: `fbuild
/// build`'s release-profile layout. Doesn't check existence — it's a
/// placeholder for the user to have built once before attaching, same as
/// any DAP `program` field.
pub(crate) fn expected_elf_path(project_path: &Path, env_name: &str) -> NormalizedPath {
    NormalizedPath::from(
        fbuild_paths::BuildLayout::new(
            project_path.to_path_buf(),
            env_name.to_string(),
            fbuild_core::BuildProfile::Release,
        )
        .resolve()
        .join("firmware.elf"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- chip mapping ----------

    #[test]
    fn maps_known_probe_rs_chips() {
        assert_eq!(probe_rs_chip_for_mcu("rp2040"), Some("RP2040"));
        assert_eq!(probe_rs_chip_for_mcu("RP2040"), Some("RP2040"));
        assert_eq!(probe_rs_chip_for_mcu("rp2350"), Some("RP235x"));
        assert_eq!(probe_rs_chip_for_mcu("imxrt1062"), Some("MIMXRT1060"));
        assert_eq!(probe_rs_chip_for_mcu("nrf52840"), Some("nRF52840_xxAA"));
        assert_eq!(probe_rs_chip_for_mcu("stm32f103c8t6"), Some("STM32F103C8"));
        assert_eq!(probe_rs_chip_for_mcu("stm32f103c8"), Some("STM32F103C8"));
    }

    #[test]
    fn unmapped_mcus_return_none() {
        for mcu in [
            "atmega328p",
            "atmega2560",
            "esp32",
            "esp32s3",
            "esp8266",
            "attiny85",
            "",
            "totally-unknown-chip",
        ] {
            assert_eq!(probe_rs_chip_for_mcu(mcu), None, "mcu={mcu}");
        }
    }

    // ---------- unsupported note ----------

    #[test]
    fn unsupported_note_names_the_target_and_milestone() {
        let note = unsupported_debug_note("esp32dev");
        assert!(note.contains("esp32dev"));
        assert!(note.contains("not supported"));
        assert!(note.contains("probe-rs"));
    }

    // ---------- debug.json merge ----------

    #[test]
    fn merge_preserves_user_entries_and_replaces_fbuild_owned() {
        let existing = vec![
            ZedDebugEntry {
                label: "My custom debug config".to_string(),
                adapter: "CodeLLDB".to_string(),
                request: "launch".to_string(),
                program: Some("/some/path".to_string()),
                tcp_connection: ZedTcpConnection {
                    host: "127.0.0.1".to_string(),
                    port: 1234,
                },
            },
            ZedDebugEntry {
                label: "fbuild: Debug (probe-rs attach)".to_string(),
                adapter: "probe-rs".to_string(),
                request: "attach".to_string(),
                program: None,
                tcp_connection: ZedTcpConnection {
                    host: "127.0.0.1".to_string(),
                    port: 9999, // stale port
                },
            },
        ];
        let fresh = vec![build_debug_entry("esp32dev", None)];
        let merged = merge_debug_entries(&existing, &fresh);

        assert!(merged.iter().any(|e| e.label == "My custom debug config"));
        let ours = merged
            .iter()
            .find(|e| e.label == "fbuild: Debug (probe-rs attach)")
            .unwrap();
        assert_eq!(ours.tcp_connection.port, PROBE_RS_DAP_PORT);
        assert_eq!(
            merged
                .iter()
                .filter(|e| e.label == "fbuild: Debug (probe-rs attach)")
                .count(),
            1
        );
    }

    #[test]
    fn merge_is_idempotent() {
        let fresh = vec![build_debug_entry("esp32dev", None)];
        let once = merge_debug_entries(&[], &fresh);
        let twice = merge_debug_entries(&once, &fresh);
        assert_eq!(once, twice);
    }

    #[test]
    fn emit_zed_debug_preserves_user_entry_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let zed_dir = tmp.path().join(".zed");
        std::fs::create_dir_all(&zed_dir).unwrap();
        std::fs::write(
            zed_dir.join("debug.json"),
            r#"[{"label": "My custom debug config", "adapter": "CodeLLDB", "request": "launch", "tcp_connection": {"host": "127.0.0.1", "port": 1}}]"#,
        )
        .unwrap();

        let path = emit_zed_debug(tmp.path(), "esp32dev", None).unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        let entries: Vec<ZedDebugEntry> = serde_json::from_str(&content).unwrap();
        assert!(entries.iter().any(|e| e.label == "My custom debug config"));
        assert!(
            entries
                .iter()
                .any(|e| e.label == "fbuild: Debug (probe-rs attach)")
        );
    }

    #[test]
    fn emit_zed_debug_is_idempotent_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let first = emit_zed_debug(tmp.path(), "esp32dev", None).unwrap();
        let first_content = std::fs::read_to_string(&first).unwrap();
        let second = emit_zed_debug(tmp.path(), "esp32dev", None).unwrap();
        let second_content = std::fs::read_to_string(&second).unwrap();
        assert_eq!(first_content, second_content);
    }

    #[test]
    fn build_debug_entry_includes_elf_program_when_given() {
        let elf_path = format!(
            "/proj/{}/{}/esp32dev/release/firmware.elf",
            fbuild_paths::FBUILD_DIR_NAME,
            fbuild_paths::BUILD_DIR_NAME
        );
        let elf = Path::new(&elf_path);
        let entry = build_debug_entry("esp32dev", Some(elf));
        assert_eq!(entry.program.as_deref(), Some(elf_path.as_str()));
    }

    #[test]
    fn build_debug_entry_omits_program_when_unresolvable() {
        let entry = build_debug_entry("esp32dev", None);
        assert_eq!(entry.program, None);
    }

    // ---------- expected elf path ----------

    #[test]
    fn expected_elf_path_points_at_release_firmware() {
        let tmp = tempfile::tempdir().unwrap();
        let path = expected_elf_path(tmp.path(), "rpipico");
        assert!(path.ends_with("firmware.elf"));
        assert!(path.to_string_lossy().contains("rpipico"));
    }
}
