//! Board configuration loaders.
//!
//! Implements [`BoardConfig::from_boards_txt`] (Arduino boards.txt format) and
//! [`BoardConfig::from_board_id`] (built-in JSON database), along with the
//! `boards.txt` line parser they share.

use std::collections::HashMap;
use std::path::Path;

use super::db::{board_id_to_board_define, get_board_debug_tools, get_board_defaults};
use super::types::BoardConfig;

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
            vid: get("vid"),
            pid: get("pid"),
            extra_flags: get("extra_flags"),
            upload_protocol: get("upload.protocol")
                .or_else(|| props.get("upload.protocol").cloned()),
            upload_speed: get("upload.speed").or_else(|| props.get("upload.speed").cloned()),
            max_flash: get("maximum_size")
                .or_else(|| props.get("maximum_size").cloned())
                .and_then(|s| s.parse().ok()),
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
            platform_str: get("platform_str"),
            debug_tools: None, // boards.txt format does not contain debug metadata
        })
    }

    /// Load board config from built-in defaults.
    pub fn from_board_id(
        board_id: &str,
        overrides: &HashMap<String, String>,
    ) -> fbuild_core::Result<Self> {
        let defaults = get_board_defaults(board_id).ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError(format!(
                "unknown board '{}' (no built-in defaults)",
                board_id
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
            max_flash: overrides
                .get("maximum_size")
                .and_then(|s| s.parse().ok())
                .or_else(|| defaults.get("maximum_size").and_then(|s| s.parse().ok())),
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
            platform_str: defaults.get("platform_str").cloned(),
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
