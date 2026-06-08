//! Embedded board database and default-extraction helpers.
//!
//! Loads PlatformIO registry JSON files baked into the binary via
//! [`include_dir`], resolves common board-id aliases, and projects the
//! relevant fields into a flat `HashMap<String, String>` of defaults
//! consumed by [`BoardConfig::from_board_id`].

use std::collections::HashMap;

use super::types::DebugToolMeta;

/// Embedded board database — 1609 boards from PlatformIO registry JSON files.
///
/// Loaded once on first access via `OnceLock`. Each entry maps board_id → JSON object
/// with fields: id, name, mcu, platform, fcpu, ram, rom, etc.
static BOARD_DB: std::sync::OnceLock<HashMap<String, serde_json::Value>> =
    std::sync::OnceLock::new();

static BOARDS_DIR: include_dir::Dir =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/assets/boards/json");

pub(super) fn get_board_db() -> &'static HashMap<String, serde_json::Value> {
    BOARD_DB.get_or_init(|| {
        let mut db = HashMap::new();
        for file in BOARDS_DIR.files() {
            let Some(stem) = file.path().file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if file.path().extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(contents) = file.contents_utf8() else {
                continue;
            };
            match serde_json::from_str(contents) {
                Ok(value) => {
                    db.insert(stem.to_string(), value);
                }
                Err(e) => {
                    tracing::error!("failed to parse board {}: {}", stem, e);
                }
            }
        }
        db
    })
}

/// Convert a board_id like "uno" to a board define like "UNO".
pub(super) fn board_id_to_board_define(board_id: &str) -> String {
    board_id.to_uppercase().replace('-', "_")
}

/// Common board aliases mapping short names to JSON board_ids.
fn resolve_board_alias(board_id: &str) -> &str {
    match board_id {
        "mega" => "megaatmega2560",
        "nano" | "nanoatmega328" => "nanoatmega328",
        "pico" | "rpipico" => "rpipico",
        "picow" | "rpipicow" => "rpipicow",
        "pico2" | "rpipico2" => "rpipico2",
        "esp32c3" => "esp32-c3-devkitm-1",
        "esp32c6" => "esp32-c6-devkitm-1",
        "esp32s3" => "esp32-s3-devkitc-1",
        "ch32l103" => "genericCH32L103C8T6",
        "ch32v003" => "genericCH32V003F4P6",
        "ch32v006" => "genericCH32V006K8U6",
        "ch32v103" => "genericCH32V103C8T6",
        "ch32v203" => "genericCH32V203C8T6",
        "ch32v208" => "genericCH32V208WBU6",
        "ch32v303" => "genericCH32V303VCT6",
        "ch32v307" => "genericCH32V307VCT6",
        "ch32x035" => "genericCH32X035C8T6",
        "adafruit_grand_central_m4" => "adafruit_grandcentral_m4",
        other => other,
    }
}

/// Extract debug tools from a board JSON entry's `debug.tools` section.
pub(super) fn get_board_debug_tools(board_id: &str) -> Option<HashMap<String, DebugToolMeta>> {
    let db = get_board_db();
    let resolved = resolve_board_alias(board_id);
    let entry = db.get(board_id).or_else(|| db.get(resolved))?;

    let tools = entry
        .get("debug")
        .and_then(|d| d.get("tools"))
        .and_then(|t| t.as_object())?;

    if tools.is_empty() {
        return None;
    }

    let mut result = HashMap::new();
    for (name, meta) in tools {
        let onboard = meta
            .get("onboard")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let default = meta
            .get("default")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        result.insert(name.clone(), DebugToolMeta { onboard, default });
    }

    Some(result)
}

/// Resolve board defaults with an optional project-local fallback.
///
/// Lookup order:
/// 1. Built-in board database (enriched JSON baked into the binary).
/// 2. `<project_dir>/boards/<board_id>.json` (PlatformIO-style project-local
///    board manifest), when `project_dir` is provided.
///
/// This matches PlatformIO's behavior of auto-discovering project-local
/// board manifests so well-formed `platformio.ini` projects that ship a
/// `boards/` directory work without per-board upstream changes.
///
/// Precedence is fallback-only: built-in wins, project-local only fills
/// the gap. Project-local override of a bundled board is intentionally
/// not supported here (open a separate feature request if needed).
pub(super) fn get_board_defaults_with_project_dir(
    board_id: &str,
    project_dir: Option<&std::path::Path>,
) -> Option<HashMap<String, String>> {
    // 1. Bundled DB lookup.
    let db = get_board_db();
    let resolved = resolve_board_alias(board_id);
    if let Some(entry) = db.get(board_id).or_else(|| db.get(resolved)) {
        return Some(flatten_board_entry(entry, board_id));
    }

    // 2. Project-local fallback: <project_dir>/boards/<board_id>.json
    //    Use the original (non-aliased) board_id only: aliases are a
    //    bundled-DB convenience, not something users override locally.
    if let Some(dir) = project_dir {
        let path = dir.join("boards").join(format!("{}.json", board_id));
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
                Ok(value) => return Some(flatten_board_entry(&value, board_id)),
                Err(e) => tracing::warn!(
                    "project-local board {} at {} failed to parse: {}",
                    board_id,
                    path.display(),
                    e
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                "project-local board {} at {} unreadable: {}",
                board_id,
                path.display(),
                e
            ),
        }
    }

    None
}

/// Project a board JSON entry (enriched format OR raw PlatformIO format)
/// into the flat `HashMap<String, String>` consumed by [`BoardConfig::from_board_id`].
///
/// Enriched format has top-level `mcu`, `fcpu`, `ram`, `rom`, `platform`.
/// PlatformIO project-local boards typically only have `build.mcu`,
/// `build.f_cpu`, `upload.maximum_ram_size`, `upload.maximum_size`. Where
/// a top-level field is missing we fall back to the PIO-style nested
/// location so both shapes work uniformly.
fn flatten_board_entry(entry: &serde_json::Value, board_id: &str) -> HashMap<String, String> {
    let mut d = HashMap::new();
    let build = entry.get("build").and_then(|v| v.as_object());

    if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
        d.insert("name".into(), name.to_string());
    }

    // mcu: enriched top-level wins; fall back to build.mcu (PIO format).
    let mcu = entry
        .get("mcu")
        .and_then(|v| v.as_str())
        .or_else(|| build.and_then(|b| b.get("mcu")).and_then(|v| v.as_str()))
        .unwrap_or("unknown")
        .to_lowercase();
    d.insert("mcu".into(), mcu.clone());

    // f_cpu: enriched top-level `fcpu` (u64) wins; fall back to PIO
    // `build.f_cpu` (string, may already carry the trailing "L").
    if let Some(fcpu) = entry.get("fcpu").and_then(|v| v.as_u64()) {
        d.insert("f_cpu".into(), format!("{}L", fcpu));
    } else if let Some(f_cpu_str) = build.and_then(|b| b.get("f_cpu")).and_then(|v| v.as_str()) {
        d.insert("f_cpu".into(), f_cpu_str.to_string());
    }

    // ram/rom: enriched top-level wins; fall back to PIO `upload.maximum_ram_size`
    // and `upload.maximum_size` respectively.
    let upload = entry.get("upload").and_then(|v| v.as_object());
    if let Some(ram) = entry.get("ram").and_then(|v| v.as_u64()) {
        d.insert("maximum_data_size".into(), ram.to_string());
    } else if let Some(ram) = upload
        .and_then(|u| u.get("maximum_ram_size"))
        .and_then(|v| v.as_u64())
    {
        d.insert("maximum_data_size".into(), ram.to_string());
    }

    if let Some(rom) = entry.get("rom").and_then(|v| v.as_u64()) {
        d.insert("maximum_size".into(), rom.to_string());
    } else if let Some(rom) = upload
        .and_then(|u| u.get("maximum_size"))
        .and_then(|v| v.as_u64())
    {
        d.insert("maximum_size".into(), rom.to_string());
    }

    if let Some(platform) = entry.get("platform").and_then(|v| v.as_str()) {
        d.insert("platform_str".into(), platform.to_string());
    }

    // Data-driven: read build section from enriched JSON
    if let Some(build) = entry.get("build").and_then(|v| v.as_object()) {
        if let Some(core) = build.get("core").and_then(|v| v.as_str()) {
            d.insert("core".into(), core.to_string());
        }
        if let Some(board) = build.get("board").and_then(|v| v.as_str()) {
            d.insert("board".into(), board.to_string());
        }
        if let Some(variant) = build.get("variant").and_then(|v| v.as_str()) {
            d.insert("variant".into(), variant.to_string());
        }
        if let Some(variant_h) = build.get("variant_h").and_then(|v| v.as_str()) {
            d.insert("variant_h".into(), variant_h.to_string());
        }
        if let Some(chip_variant) = build.get("chip_variant").and_then(|v| v.as_str()) {
            d.insert("chip_variant".into(), chip_variant.to_string());
        }
        if let Some(flags) = build.get("extra_flags").and_then(|v| v.as_str()) {
            d.insert("extra_flags".into(), flags.to_string());
        }
        if let Some(vid) = build.get("vid").and_then(|v| v.as_str()) {
            d.insert("vid".into(), vid.to_string());
        }
        if let Some(pid) = build.get("pid").and_then(|v| v.as_str()) {
            d.insert("pid".into(), pid.to_string());
        }
        if let Some(flash_mode) = build.get("flash_mode").and_then(|v| v.as_str()) {
            d.insert("flash_mode".into(), flash_mode.to_string());
        }
        if let Some(memory_type) = build.get("memory_type").and_then(|v| v.as_str()) {
            d.insert("memory_type".into(), memory_type.to_string());
        }
        if let Some(psram_type) = build.get("psram_type").and_then(|v| v.as_str()) {
            d.insert("psram_type".into(), psram_type.to_string());
        }
        if let Some(f_flash) = build.get("f_flash").and_then(|v| v.as_str()) {
            d.insert("f_flash".into(), f_flash.to_string());
        }
        if let Some(f_image) = build.get("f_image").and_then(|v| v.as_str()) {
            d.insert("f_image".into(), f_image.to_string());
        }
        if let Some(flash_size) = build.get("flash_size").and_then(|v| v.as_str()) {
            d.insert("flash_size".into(), flash_size.to_string());
        }
        if let Some(cmsis_dsp_lib) = build.get("cmsis_dsp_lib").and_then(|v| v.as_str()) {
            d.insert("cmsis_dsp_lib".into(), cmsis_dsp_lib.to_string());
        }
        // Arduino sub-fields
        if let Some(arduino) = build.get("arduino").and_then(|v| v.as_object()) {
            if let Some(ldscript) = arduino.get("ldscript").and_then(|v| v.as_str()) {
                d.insert("ldscript".into(), ldscript.to_string());
            }
            if let Some(partitions) = arduino.get("partitions").and_then(|v| v.as_str()) {
                d.insert("partitions".into(), partitions.to_string());
            }
            if let Some(memory_type) = arduino.get("memory_type").and_then(|v| v.as_str()) {
                d.insert("memory_type".into(), memory_type.to_string());
            }
            // Core-specific overrides: build.arduino.<core_name>.variant
            // e.g. build.arduino.openwch.variant = "CH32V00x/CH32V003F4"
            if let Some(core_name) = build.get("core").and_then(|v| v.as_str()) {
                if let Some(core_obj) = arduino.get(core_name).and_then(|v| v.as_object()) {
                    if let Some(variant) = core_obj.get("variant").and_then(|v| v.as_str()) {
                        d.insert("variant".into(), variant.to_string());
                    }
                    if let Some(vh) = core_obj.get("variant_h").and_then(|v| v.as_str()) {
                        d.insert("variant_h".into(), vh.to_string());
                    }
                }
            }
        }
    }

    // Data-driven: read upload section from enriched JSON
    if let Some(upload) = entry.get("upload").and_then(|v| v.as_object()) {
        if let Some(protocol) = upload.get("protocol").and_then(|v| v.as_str()) {
            d.insert("upload.protocol".into(), protocol.to_string());
        }
        if let Some(speed) = upload.get("speed").and_then(|v| v.as_u64()) {
            d.insert("upload.speed".into(), speed.to_string());
        }
        if let Some(flash_size) = upload.get("flash_size").and_then(|v| v.as_str()) {
            d.insert("upload.flash_size".into(), flash_size.to_string());
        }
        if let Some(maximum_size) = upload.get("maximum_size").and_then(|v| v.as_u64()) {
            d.insert("upload.maximum_size".into(), maximum_size.to_string());
        }
    }

    // Fallback for unenriched boards: derive from platform
    if !d.contains_key("core") {
        d.insert("core".into(), "arduino".into());
    }
    if !d.contains_key("variant") {
        // For ESP32 boards, the Arduino framework uses the MCU name as the
        // variant directory name (e.g., variants/esp32c6/).  Fall back to
        // MCU rather than board_id so builds find pins_arduino.h.
        let fallback = if d.get("core").is_some_and(|c| c == "esp32") {
            d.get("mcu")
                .cloned()
                .unwrap_or_else(|| board_id.to_string())
        } else {
            board_id.to_string()
        };
        d.insert("variant".into(), fallback);
    }
    d.entry("board".into())
        .or_insert_with(|| board_id_to_board_define(board_id));

    d
}
