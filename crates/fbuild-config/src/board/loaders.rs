//! Board configuration loaders.
//!
//! Implements [`BoardConfig::from_boards_txt`] (Arduino boards.txt format) and
//! [`BoardConfig::from_board_id`] (built-in JSON database), along with the
//! `boards.txt` line parser they share.

use std::collections::HashMap;
use std::path::Path;

use super::db::{
    board_id_to_board_define, get_board_debug_tools, get_board_defaults_with_project_dir,
};
use super::types::BoardConfig;

fn parse_flash_size_bytes(raw: &str) -> Option<u64> {
    let value = raw.trim().replace('_', "");
    if value.is_empty() {
        return None;
    }
    if let Ok(bytes) = value.parse::<u64>() {
        return Some(bytes);
    }

    let lower = value.to_ascii_lowercase();
    let (number, multiplier) = lower
        .strip_suffix("mb")
        .map(|n| (n, 1024_u64 * 1024))
        .or_else(|| lower.strip_suffix('m').map(|n| (n, 1024_u64 * 1024)))
        .or_else(|| lower.strip_suffix("kb").map(|n| (n, 1024_u64)))
        .or_else(|| lower.strip_suffix('k').map(|n| (n, 1024_u64)))?;

    number
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
}

fn first_parsed_size<'a>(
    maps: impl IntoIterator<Item = &'a HashMap<String, String>>,
    keys: &[&str],
) -> Option<u64> {
    for map in maps {
        for key in keys {
            if let Some(value) = map.get(*key).and_then(|s| parse_flash_size_bytes(s)) {
                return Some(value);
            }
        }
    }
    None
}

fn resolve_max_flash(
    overrides: &HashMap<String, String>,
    defaults: &HashMap<String, String>,
) -> Option<u64> {
    first_parsed_size(
        [overrides],
        &[
            "maximum_size",
            "upload.maximum_size",
            "flash_size",
            "upload.flash_size",
        ],
    )
    .or_else(|| {
        first_parsed_size(
            [defaults],
            &[
                "maximum_size",
                "upload.maximum_size",
                "flash_size",
                "upload.flash_size",
            ],
        )
    })
}

impl BoardConfig {
    /// Load board config from a boards.txt file.
    ///
    /// Format: `uno.build.mcu=atmega328p`, `uno.name=Arduino Uno`
    pub fn from_boards_txt(
        path: &Path,
        board_id: &str,
        overrides: &HashMap<String, String>,
    ) -> fbuild_core::Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            fbuild_core::FbuildError::ConfigError(format!(
                "failed to read boards.txt at {}: {}",
                path.display(),
                e
            ))
        })?;

        let props = parse_boards_txt(&content, board_id);
        if props.is_empty() {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "board '{}' not found in {}",
                board_id,
                path.display()
            )));
        }

        let get = |key: &str| -> Option<String> {
            overrides
                .get(key)
                .cloned()
                .or_else(|| props.get(key).cloned())
        };

        let name = get("name").unwrap_or_else(|| board_id.to_string());
        let mcu = get("mcu").ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError(format!(
                "board '{}' missing required field 'mcu'",
                board_id
            ))
        })?;

        // For ESP32 chips we deliberately drop the boards.txt `flash_mode`
        // field and let downstream consumers fall back to the per-MCU
        // default ("dio"). See the equivalent comment in `from_board_id`
        // for the rationale (ESP32-S3 QIE-bit unreliability + bootloader
        // ROM that requires DIO).
        let is_esp32_family = mcu.starts_with("esp32");

        Ok(Self {
            name,
            mcu,
            f_cpu: get("f_cpu").unwrap_or_else(|| "16000000L".to_string()),
            board: get("board")
                .or_else(|| props.get("board").cloned())
                .unwrap_or_else(|| board_id_to_board_define(board_id)),
            core: get("core").unwrap_or_else(|| "arduino".to_string()),
            variant: get("variant").unwrap_or_else(|| "standard".to_string()),
            variant_h: get("variant_h"),
            chip_variant: get("chip_variant"),
            vid: get("vid"),
            pid: get("pid"),
            extra_flags: get("extra_flags"),
            upload_protocol: get("upload.protocol")
                .or_else(|| props.get("upload.protocol").cloned()),
            upload_speed: get("upload.speed").or_else(|| props.get("upload.speed").cloned()),
            max_flash: resolve_max_flash(overrides, &props),
            max_ram: get("maximum_data_size")
                .or_else(|| props.get("maximum_data_size").cloned())
                .and_then(|s| s.parse().ok()),
            flash_mode: if is_esp32_family {
                overrides.get("flash_mode").cloned()
            } else {
                get("flash_mode")
            },
            memory_type: if is_esp32_family {
                overrides
                    .get("memory_type")
                    .cloned()
                    .or_else(|| get("memory_type"))
            } else {
                get("memory_type")
            },
            psram_type: if is_esp32_family {
                overrides
                    .get("psram_type")
                    .cloned()
                    .or_else(|| get("psram_type"))
            } else {
                get("psram_type")
            },
            f_flash: get("f_flash"),
            f_image: get("f_image"),
            partitions: get("partitions"),
            ldscript: get("ldscript"),
            openocd_target: get("openocd_target"),
            platform_str: get("platform_str"),
            cmsis_dsp_lib: get("cmsis_dsp_lib"),
            debug_tools: None, // boards.txt format does not contain debug metadata
        })
    }

    /// Load board config from built-in defaults.
    pub fn from_board_id(
        board_id: &str,
        overrides: &HashMap<String, String>,
    ) -> fbuild_core::Result<Self> {
        Self::from_board_id_in_project(board_id, overrides, None)
    }

    /// Load `board_id`, falling back to a known-good `default_board_id`
    /// (e.g. `"esp32dev"`, `"uno"`, `"teensy41"`) when the primary id is
    /// unknown, carrying the same `overrides` through to the fallback.
    ///
    /// `default_board_id` is expected to be a compile-time platform default
    /// that always exists in the board database, so a failure to resolve it
    /// is a programming error and panics rather than being swallowed.
    pub fn from_board_id_or_default(
        board_id: &str,
        default_board_id: &str,
        overrides: &HashMap<String, String>,
    ) -> Self {
        Self::from_board_id(board_id, overrides).unwrap_or_else(|_| {
            Self::from_board_id(default_board_id, overrides)
                .unwrap_or_else(|e| panic!("default board '{default_board_id}' must resolve: {e}"))
        })
    }

    /// Load board config from built-in defaults with a project-local fallback.
    ///
    /// When the built-in board database has no entry for `board_id`, fall
    /// back to `<project_dir>/boards/<board_id>.json` (PlatformIO-style
    /// project-local board manifest). This matches PlatformIO's behavior
    /// of auto-discovering project-local board manifests next to
    /// `platformio.ini`.
    ///
    /// Pass `None` for `project_dir` to disable the fallback (equivalent
    /// to [`Self::from_board_id`]).
    pub fn from_board_id_in_project(
        board_id: &str,
        overrides: &HashMap<String, String>,
        project_dir: Option<&std::path::Path>,
    ) -> fbuild_core::Result<Self> {
        let defaults =
            get_board_defaults_with_project_dir(board_id, project_dir).ok_or_else(|| {
                let suffix = match project_dir {
                    Some(d) => format!(" (also checked {}/boards/{}.json)", d.display(), board_id),
                    None => String::new(),
                };
                fbuild_core::FbuildError::ConfigError(format!(
                    "unknown board '{}' (no built-in defaults){}",
                    board_id, suffix
                ))
            })?;

        let get = |key: &str, default: &str| -> String {
            overrides
                .get(key)
                .cloned()
                .unwrap_or_else(|| defaults.get(key).cloned().unwrap_or(default.to_string()))
        };

        // Determine if this is an ESP32-family chip — used to ignore the
        // board JSON's `flash_mode` field below. We have to compute this
        // here (before constructing Self) because the resolution of
        // `flash_mode` happens inside the struct literal.
        let resolved_mcu = get("mcu", "unknown");
        let is_esp32_family = resolved_mcu.starts_with("esp32");

        Ok(Self {
            name: get("name", board_id),
            mcu: get("mcu", "unknown"),
            f_cpu: get("f_cpu", "16000000L"),
            board: get("board", &board_id_to_board_define(board_id)),
            core: get("core", "arduino"),
            variant: get("variant", "standard"),
            variant_h: overrides
                .get("variant_h")
                .cloned()
                .or_else(|| defaults.get("variant_h").cloned()),
            chip_variant: overrides
                .get("chip_variant")
                .cloned()
                .or_else(|| defaults.get("chip_variant").cloned()),
            vid: overrides
                .get("vid")
                .cloned()
                .or_else(|| defaults.get("vid").cloned()),
            pid: overrides
                .get("pid")
                .cloned()
                .or_else(|| defaults.get("pid").cloned()),
            extra_flags: overrides
                .get("extra_flags")
                .cloned()
                .or_else(|| defaults.get("extra_flags").cloned()),
            upload_protocol: overrides
                .get("upload.protocol")
                .cloned()
                .or_else(|| defaults.get("upload.protocol").cloned()),
            upload_speed: overrides
                .get("upload.speed")
                .cloned()
                .or_else(|| defaults.get("upload.speed").cloned()),
            max_flash: resolve_max_flash(overrides, &defaults),
            max_ram: overrides
                .get("maximum_data_size")
                .and_then(|s| s.parse().ok())
                .or_else(|| {
                    defaults
                        .get("maximum_data_size")
                        .and_then(|s| s.parse().ok())
                }),
            // Flash mode resolution:
            //   - For ESP32 chips, IGNORE the board JSON's flash_mode field.
            //     Many board JSONs ship `flash_mode: qio` because the flash
            //     chip *supports* QIO, but ESP32-S3's QIE-bit init is
            //     unreliable on real hardware. The MCU-level default
            //     (`default_flash_mode` in fbuild-build's esp32 configs) is
            //     "dio" for the entire ESP32 family — that's the safe value
            //     to use unless the user explicitly opts in via the env
            //     section's `board_build.flash_mode = qio`.
            //   - For non-ESP32 chips, the existing behaviour is preserved
            //     (env override → board JSON → None).
            //   - Either way, downstream code that needs an effective value
            //     when this is `None` should fall back to
            //     `mcu_config.default_flash_mode()`.
            flash_mode: if is_esp32_family {
                overrides.get("flash_mode").cloned()
            } else {
                overrides
                    .get("flash_mode")
                    .cloned()
                    .or_else(|| defaults.get("flash_mode").cloned())
            },
            memory_type: overrides
                .get("memory_type")
                .cloned()
                .or_else(|| defaults.get("memory_type").cloned()),
            psram_type: overrides
                .get("psram_type")
                .cloned()
                .or_else(|| defaults.get("psram_type").cloned()),
            f_flash: overrides
                .get("f_flash")
                .cloned()
                .or_else(|| defaults.get("f_flash").cloned()),
            f_image: overrides
                .get("f_image")
                .cloned()
                .or_else(|| defaults.get("f_image").cloned()),
            partitions: overrides
                .get("partitions")
                .cloned()
                .or_else(|| defaults.get("partitions").cloned()),
            ldscript: overrides
                .get("ldscript")
                .cloned()
                .or_else(|| defaults.get("ldscript").cloned()),
            openocd_target: overrides
                .get("openocd_target")
                .cloned()
                .or_else(|| defaults.get("openocd_target").cloned()),
            platform_str: defaults.get("platform_str").cloned(),
            cmsis_dsp_lib: overrides
                .get("cmsis_dsp_lib")
                .cloned()
                .or_else(|| defaults.get("cmsis_dsp_lib").cloned()),
            debug_tools: get_board_debug_tools(board_id),
        })
    }
}

/// Parse boards.txt content for a specific board_id.
///
/// Format: `board_id.key=value`, with `build.` and `upload.` prefixes.
pub(super) fn parse_boards_txt(content: &str, board_id: &str) -> HashMap<String, String> {
    let mut props = HashMap::new();
    let prefix = format!("{}.", board_id);

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            if let Some(eq_pos) = rest.find('=') {
                let key = rest[..eq_pos].trim();
                let value = rest[eq_pos + 1..].trim();

                // Strip build. and upload. prefixes for convenience
                let normalized_key = key
                    .strip_prefix("build.")
                    .or_else(|| key.strip_prefix("upload."))
                    .unwrap_or(key);

                // Keep both the original and stripped key
                props.insert(normalized_key.to_string(), value.to_string());
                if normalized_key != key {
                    props.insert(key.to_string(), value.to_string());
                }
            }
        }
    }

    props
}
