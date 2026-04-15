//! Board configuration from boards.txt and built-in defaults.
//!
//! Supports:
//! - Loading from Arduino boards.txt format
//! - Built-in defaults for common boards
//! - Field overrides from platformio.ini board_build.* keys
//! - Preprocessor defines generation
//! - Include path resolution

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Esp32QemuPsramConfig {
    pub size_mib: u32,
    pub is_octal: bool,
}

/// Metadata for a single debug tool entry from the board JSON `debug.tools` section.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DebugToolMeta {
    /// Whether the tool is built into the board (no external hardware needed).
    #[serde(default)]
    pub onboard: bool,
    /// Whether this is the board's default debug tool.
    #[serde(default)]
    pub default: bool,
}

/// Known emulator/simulator tool names that can run firmware without hardware.
const EMULATOR_TOOL_NAMES: &[&str] = &["simavr", "qemu", "renode", "ovpsim", "verilator"];

/// Board configuration loaded from boards.txt or built-in defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardConfig {
    pub name: String,
    pub mcu: String,
    pub f_cpu: String,
    pub board: String,
    pub core: String,
    pub variant: String,
    /// Variant header override for frameworks that use `#include VARIANT_H`
    pub variant_h: Option<String>,
    /// USB vendor ID (optional)
    pub vid: Option<String>,
    /// USB product ID (optional)
    pub pid: Option<String>,
    /// Extra build flags from board definition
    pub extra_flags: Option<String>,
    /// Upload protocol (e.g. "arduino", "esptool", "teensy-gui")
    pub upload_protocol: Option<String>,
    /// Upload speed
    pub upload_speed: Option<String>,
    /// Maximum flash size in bytes
    pub max_flash: Option<u64>,
    /// Maximum RAM size in bytes
    pub max_ram: Option<u64>,
    /// Flash mode (e.g. "dio", "qio") — ESP32 boards
    pub flash_mode: Option<String>,
    /// Memory profile (e.g. "qio_qspi", "qio_opi") - ESP32 boards
    pub memory_type: Option<String>,
    /// PSRAM type (e.g. "qspi", "opi") - ESP32 boards
    pub psram_type: Option<String>,
    /// Flash frequency (e.g. "80000000L") — ESP32 boards
    pub f_flash: Option<String>,
    /// Image flash frequency override (e.g. "48000000L") — used by esptool when
    /// the board's actual SPI clock (`f_flash`) doesn't match a valid esptool frequency.
    /// PlatformIO calls this `build.f_image`. When present, this takes priority over
    /// `f_flash` for esptool's `--flash-freq` argument.
    pub f_image: Option<String>,
    /// Partition table file (e.g. "default_8MB.csv") — ESP32 boards
    pub partitions: Option<String>,
    /// Linker script (e.g. "esp32s3_out.ld")
    pub ldscript: Option<String>,
    /// Platform string from board JSON (e.g. "atmelmegaavr", "atmelavr")
    pub platform_str: Option<String>,
    /// Debug tools from board JSON `debug.tools` section.
    /// Maps tool name (e.g. "simavr", "qemu", "renode") to its metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_tools: Option<HashMap<String, DebugToolMeta>>,
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

    /// Returns emulator/simulator tools available for this board.
    ///
    /// Filters `debug_tools` to only include known software emulators
    /// (simavr, qemu, renode, ovpsim, verilator), excluding hardware debug probes.
    pub fn emulators(&self) -> HashMap<&str, &DebugToolMeta> {
        let Some(ref tools) = self.debug_tools else {
            return HashMap::new();
        };
        tools
            .iter()
            .filter(|(name, _)| EMULATOR_TOOL_NAMES.contains(&name.as_str()))
            .map(|(name, meta)| (name.as_str(), meta))
            .collect()
    }

    /// Check whether this board supports a specific emulator tool.
    pub fn has_emulator(&self, tool_name: &str) -> bool {
        self.debug_tools
            .as_ref()
            .is_some_and(|tools| tools.contains_key(tool_name))
            && EMULATOR_TOOL_NAMES.contains(&tool_name)
    }

    /// Resolve the effective ESP32 SDK memory profile used for variant headers/libs.
    ///
    /// This keeps the SDK `sdkconfig.h` and memory-profile libraries aligned
    /// with the repo's effective flash-mode policy. Boards that explicitly use
    /// OPI flash keep the `opi` flash-half because that represents a distinct
    /// bus type rather than an optional fast-read mode.
    pub fn effective_esp32_memory_type(&self, default_flash_mode: &str) -> Option<String> {
        if !self.mcu.starts_with("esp32") {
            return None;
        }

        let effective_flash_mode = self
            .flash_mode
            .as_deref()
            .unwrap_or(default_flash_mode)
            .to_ascii_lowercase();

        let (flash_half, psram_half) = if let Some(memory_type) = self.memory_type.as_deref() {
            if let Some((flash, psram)) = memory_type.split_once('_') {
                (
                    Some(flash.to_ascii_lowercase()),
                    Some(psram.to_ascii_lowercase()),
                )
            } else {
                (Some(memory_type.to_ascii_lowercase()), None)
            }
        } else {
            (None, None)
        };

        let resolved_flash = match flash_half.as_deref() {
            Some("opi") => "opi".to_string(),
            _ => effective_flash_mode,
        };
        let resolved_psram = psram_half
            .or_else(|| self.psram_type.as_deref().map(|s| s.to_ascii_lowercase()))
            .unwrap_or_else(|| "qspi".to_string());

        Some(format!("{}_{}", resolved_flash, resolved_psram))
    }

    pub fn qemu_esp32_psram_config(&self) -> Option<Esp32QemuPsramConfig> {
        let has_psram = self
            .extra_flags
            .as_deref()
            .is_some_and(|flags| extra_flags_contain_define(flags, "BOARD_HAS_PSRAM"))
            || self.psram_type.is_some();
        if !has_psram {
            return None;
        }

        let is_octal = self
            .psram_type
            .as_deref()
            .is_some_and(|psram| psram.eq_ignore_ascii_case("opi"))
            || self
                .memory_type
                .as_deref()
                .is_some_and(|memory| memory.ends_with("_opi"));
        let size_mib = infer_psram_size_mib(&self.name).unwrap_or(if is_octal { 8 } else { 2 });

        Some(Esp32QemuPsramConfig { size_mib, is_octal })
    }

    /// Detect the platform from the board JSON's platform field, or fall back to MCU heuristic.
    pub fn platform(&self) -> Option<fbuild_core::Platform> {
        // Prefer explicit platform from board JSON (distinguishes AtmelMegaAvr from AtmelAvr)
        if let Some(ref p) = self.platform_str {
            if let Some(platform) = fbuild_core::Platform::from_platform_str(p) {
                return Some(platform);
            }
        }
        let mcu = self.mcu.to_lowercase();
        if mcu.starts_with("atmega") || mcu.starts_with("attiny") || mcu.starts_with("at90") {
            Some(fbuild_core::Platform::AtmelAvr)
        } else if mcu.starts_with("esp32") {
            Some(fbuild_core::Platform::Espressif32)
        } else if mcu.starts_with("esp8266") || mcu.starts_with("esp8285") {
            Some(fbuild_core::Platform::Espressif8266)
        } else if mcu.starts_with("imxrt") || mcu.starts_with("mk") {
            Some(fbuild_core::Platform::Teensy)
        } else if mcu.starts_with("rp2040") || mcu.starts_with("rp2350") {
            Some(fbuild_core::Platform::RaspberryPi)
        } else if mcu.starts_with("stm32") {
            Some(fbuild_core::Platform::Ststm32)
        } else if mcu.starts_with("nrf52") {
            Some(fbuild_core::Platform::NordicNrf52)
        } else if mcu.starts_with("at91sam") || mcu.starts_with("sam") {
            Some(fbuild_core::Platform::AtmelSam)
        } else if mcu.starts_with("ra4") || mcu.starts_with("ra6") {
            Some(fbuild_core::Platform::RenesasRa)
        } else if mcu.starts_with("ch32") {
            Some(fbuild_core::Platform::Ch32v)
        } else if mcu.starts_with("apollo3") || mcu.starts_with("ama3b") {
            Some(fbuild_core::Platform::Apollo3)
        } else {
            None
        }
    }

    /// Generate preprocessor defines for this board.
    ///
    /// Returns defines like: PLATFORMIO, F_CPU, ARDUINO, `ARDUINO_<BOARD>`, `ARDUINO_ARCH_<ARCH>`
    pub fn get_defines(&self) -> HashMap<String, String> {
        let mut defines = HashMap::new();

        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        defines.insert("F_CPU".to_string(), self.f_cpu.clone());

        // Default Arduino version. Platform-specific overrides (e.g. Teensy=10819)
        // are in MCU config JSON defines, merged by the orchestrator after this.
        defines.insert("ARDUINO".to_string(), "10808".to_string());

        defines.insert(
            format!("ARDUINO_{}", self.board.to_uppercase()),
            "1".to_string(),
        );
        // ARDUINO_BOARD and ARDUINO_VARIANT as quoted string defines.
        // Use \" escapes so GCC response files on Windows preserve the quotes
        // (bare " is treated as a word delimiter by GCC's response file parser).
        defines.insert(
            "ARDUINO_BOARD".to_string(),
            format!("\\\"{}\\\"", self.board),
        );
        defines.insert(
            "ARDUINO_VARIANT".to_string(),
            format!("\\\"{}\\\"", self.variant),
        );

        // Architecture define
        let arch = self.arch_define();
        if !arch.is_empty() {
            defines.insert(format!("ARDUINO_ARCH_{}", arch), "1".to_string());
        }

        // MCU-specific define for AVR
        let mcu_upper = self.mcu.to_uppercase();
        if mcu_upper.starts_with("ATMEGA") || mcu_upper.starts_with("ATTINY") {
            defines.insert(format!("__AVR_{}__", mcu_upper), "1".to_string());
        }

        // Teensy __MCU__ define (MCU detection, not a versioned constant)
        if matches!(self.platform(), Some(fbuild_core::Platform::Teensy))
            && mcu_upper.starts_with("IMXRT")
        {
            defines.insert(format!("__{}__", mcu_upper), "1".to_string());
        }

        // USB VID/PID defines for USB-native boards (Leonardo, Micro, etc.)
        if let Some(ref vid) = self.vid {
            defines.insert("USB_VID".to_string(), vid.clone());
        }
        if let Some(ref pid) = self.pid {
            defines.insert("USB_PID".to_string(), pid.clone());
        }

        // Extra flags
        if let Some(ref flags) = self.extra_flags {
            for flag in fbuild_core::shell_split::split(flags) {
                if let Some(define) = flag.strip_prefix("-D") {
                    if let Some(eq_pos) = define.find('=') {
                        defines.insert(
                            define[..eq_pos].to_string(),
                            define[eq_pos + 1..].to_string(),
                        );
                    } else {
                        defines.insert(define.to_string(), "1".to_string());
                    }
                }
            }
        }

        defines
    }

    /// Get include paths relative to a framework root directory.
    ///
    /// Returns: `[cores/<core>, variants/<variant>]`
    pub fn get_include_paths(&self, framework_root: &Path) -> Vec<PathBuf> {
        vec![
            framework_root.join("cores").join(&self.core),
            framework_root.join("variants").join(&self.variant),
        ]
    }

    fn arch_define(&self) -> String {
        match self.platform() {
            Some(fbuild_core::Platform::AtmelAvr) => "AVR".to_string(),
            Some(fbuild_core::Platform::AtmelMegaAvr) => "MEGAAVR".to_string(),
            Some(fbuild_core::Platform::Espressif32) => "ESP32".to_string(),
            Some(fbuild_core::Platform::Espressif8266) => "ESP8266".to_string(),
            Some(fbuild_core::Platform::NordicNrf52) => "NRF52".to_string(),
            Some(fbuild_core::Platform::RaspberryPi) => "RP2040".to_string(),
            Some(fbuild_core::Platform::RenesasRa) => "RENESAS".to_string(),
            Some(fbuild_core::Platform::SiliconLabs) => "SILABS".to_string(),
            Some(fbuild_core::Platform::Ststm32) => "STM32".to_string(),
            Some(fbuild_core::Platform::AtmelSam) => "SAM".to_string(),
            Some(fbuild_core::Platform::Teensy) => "TEENSY".to_string(),
            Some(fbuild_core::Platform::Ch32v) => "CH32V".to_string(),
            Some(fbuild_core::Platform::Apollo3) => "APOLLO3".to_string(),
            Some(fbuild_core::Platform::Wasm) | None => String::new(),
        }
    }
}

/// Parse boards.txt content for a specific board_id.
///
/// Format: `board_id.key=value`, with `build.` and `upload.` prefixes.
fn parse_boards_txt(content: &str, board_id: &str) -> HashMap<String, String> {
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

/// Convert a board_id like "uno" to a board define like "UNO".
fn board_id_to_board_define(board_id: &str) -> String {
    board_id.to_uppercase().replace('-', "_")
}

fn extra_flags_contain_define(extra_flags: &str, define: &str) -> bool {
    extra_flags.split_whitespace().any(|flag| {
        let Some(raw) = flag.strip_prefix("-D") else {
            return false;
        };
        raw.split_once('=').map_or(raw, |(name, _)| name) == define
    })
}

fn infer_psram_size_mib(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();

    for size in [32_u32, 16, 8, 4, 2] {
        if upper.contains(&format!("{size} MB PSRAM")) {
            return Some(size);
        }
    }

    for (marker, size) in [
        ("R32", 32_u32),
        ("R16", 16),
        ("R8", 8),
        ("R4", 4),
        ("R2", 2),
    ] {
        if upper.contains(marker) {
            return Some(size);
        }
    }

    None
}

/// Embedded board database — 1609 boards from PlatformIO registry JSON files.
///
/// Loaded once on first access via `OnceLock`. Each entry maps board_id → JSON object
/// with fields: id, name, mcu, platform, fcpu, ram, rom, etc.
static BOARD_DB: std::sync::OnceLock<HashMap<String, serde_json::Value>> =
    std::sync::OnceLock::new();

static BOARDS_DIR: include_dir::Dir =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/assets/boards/json");

fn get_board_db() -> &'static HashMap<String, serde_json::Value> {
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
fn get_board_debug_tools(board_id: &str) -> Option<HashMap<String, DebugToolMeta>> {
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

fn get_board_defaults(board_id: &str) -> Option<HashMap<String, String>> {
    let db = get_board_db();
    let resolved = resolve_board_alias(board_id);
    let entry = db.get(board_id).or_else(|| db.get(resolved))?;

    let mut d = HashMap::new();

    if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
        d.insert("name".into(), name.to_string());
    }

    let mcu = entry
        .get("mcu")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_lowercase();
    d.insert("mcu".into(), mcu.clone());

    if let Some(fcpu) = entry.get("fcpu").and_then(|v| v.as_u64()) {
        d.insert("f_cpu".into(), format!("{}L", fcpu));
    }

    if let Some(ram) = entry.get("ram").and_then(|v| v.as_u64()) {
        d.insert("maximum_data_size".into(), ram.to_string());
    }

    if let Some(rom) = entry.get("rom").and_then(|v| v.as_u64()) {
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
        if let Some(variant) = build.get("variant").and_then(|v| v.as_str()) {
            d.insert("variant".into(), variant.to_string());
        }
        if let Some(variant_h) = build.get("variant_h").and_then(|v| v.as_str()) {
            d.insert("variant_h".into(), variant_h.to_string());
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

    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_boards_txt(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_from_board_id_uno() {
        let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        assert_eq!(config.name, "Arduino Uno");
        assert_eq!(config.mcu, "atmega328p");
        assert_eq!(config.f_cpu, "16000000L");
        assert_eq!(config.board, "UNO");
        assert_eq!(config.core, "arduino");
        assert_eq!(config.variant, "standard");
        assert_eq!(config.max_flash, Some(32256));
        assert_eq!(config.max_ram, Some(2048));
        // extra_flags from enriched JSON
        assert_eq!(config.extra_flags, Some("-DARDUINO_AVR_UNO".to_string()));
        assert_eq!(config.upload_protocol, Some("arduino".to_string()));
        assert_eq!(config.upload_speed, Some("115200".to_string()));
    }

    #[test]
    fn test_from_board_id_mega() {
        let config = BoardConfig::from_board_id("mega", &HashMap::new()).unwrap();
        assert_eq!(config.mcu, "atmega2560");
        assert_eq!(config.variant, "mega");
    }

    #[test]
    fn test_from_board_id_megaatmega2560_alias() {
        let config = BoardConfig::from_board_id("megaatmega2560", &HashMap::new()).unwrap();
        assert_eq!(config.mcu, "atmega2560");
    }

    #[test]
    fn test_ch32v_aliases() {
        let cases = [
            ("ch32l103", "ch32l103c8t6", Some("variant_CH32L103C8T6.h")),
            ("ch32v003", "ch32v003f4p6", Some("variant_CH32V003F4.h")),
            ("ch32v006", "ch32v006k8u6", Some("variant_CH32V006K8.h")),
            ("ch32v103", "ch32v103c8t6", Some("variant_CH32V103R8T6.h")),
            ("ch32v203", "ch32v203c8t6", Some("variant_CH32V203C8.h")),
            ("ch32v208", "ch32v208wbu6", Some("variant_CH32V203C8.h")),
            ("ch32v303", "ch32v303vct6", Some("variant_CH32V307VCT6.h")),
            ("ch32v307", "ch32v307vct6", Some("variant_CH32V307VCT6.h")),
            ("ch32x035", "ch32x035c8t6", Some("variant_CH32X035G8U.h")),
        ];
        for (alias, expected_mcu, expected_variant_h) in cases {
            let config = BoardConfig::from_board_id(alias, &HashMap::new()).unwrap();
            assert_eq!(
                config.mcu, expected_mcu,
                "alias '{}' should resolve to mcu '{}'",
                alias, expected_mcu
            );
            assert_eq!(
                config.variant_h.as_deref(),
                expected_variant_h,
                "alias '{}' should resolve to variant_h '{:?}'",
                alias,
                expected_variant_h
            );
        }
    }

    #[test]
    fn test_from_board_id_unknown() {
        let result = BoardConfig::from_board_id("nonexistent_board", &HashMap::new());
        assert!(result.is_err());
    }

    #[test]
    fn test_from_board_id_with_overrides() {
        let mut overrides = HashMap::new();
        overrides.insert("f_cpu".to_string(), "8000000L".to_string());
        let config = BoardConfig::from_board_id("uno", &overrides).unwrap();
        assert_eq!(config.f_cpu, "8000000L");
        assert_eq!(config.mcu, "atmega328p"); // not overridden
    }

    #[test]
    fn test_from_boards_txt_valid() {
        let f = write_boards_txt(
            "\
uno.name=Arduino Uno
uno.build.mcu=atmega328p
uno.build.f_cpu=16000000L
uno.build.board=AVR_UNO
uno.build.core=arduino
uno.build.variant=standard
uno.upload.maximum_size=32256
uno.upload.maximum_data_size=2048
",
        );
        let config = BoardConfig::from_boards_txt(f.path(), "uno", &HashMap::new()).unwrap();
        assert_eq!(config.name, "Arduino Uno");
        assert_eq!(config.mcu, "atmega328p");
        assert_eq!(config.max_flash, Some(32256));
    }

    #[test]
    fn test_from_boards_txt_nonexistent_board() {
        let f = write_boards_txt("uno.name=Arduino Uno\nuno.build.mcu=atmega328p\n");
        let result = BoardConfig::from_boards_txt(f.path(), "mega", &HashMap::new());
        assert!(result.is_err());
    }

    #[test]
    fn test_from_boards_txt_with_overrides() {
        let f = write_boards_txt(
            "\
uno.name=Arduino Uno
uno.build.mcu=atmega328p
uno.build.f_cpu=16000000L
uno.build.board=AVR_UNO
uno.build.core=arduino
uno.build.variant=standard
",
        );
        let mut overrides = HashMap::new();
        overrides.insert("f_cpu".to_string(), "8000000L".to_string());
        let config = BoardConfig::from_boards_txt(f.path(), "uno", &overrides).unwrap();
        assert_eq!(config.f_cpu, "8000000L");
    }

    #[test]
    fn test_from_boards_txt_multiple_boards() {
        let f = write_boards_txt(
            "\
uno.name=Arduino Uno
uno.build.mcu=atmega328p
mega.name=Arduino Mega 2560
mega.build.mcu=atmega2560
",
        );
        let uno = BoardConfig::from_boards_txt(f.path(), "uno", &HashMap::new()).unwrap();
        let mega = BoardConfig::from_boards_txt(f.path(), "mega", &HashMap::new()).unwrap();
        assert_eq!(uno.mcu, "atmega328p");
        assert_eq!(mega.mcu, "atmega2560");
    }

    #[test]
    fn test_from_boards_txt_with_upload_info() {
        let f = write_boards_txt(
            "\
leonardo.name=Arduino Leonardo
leonardo.build.mcu=atmega32u4
leonardo.build.vid=0x2341
leonardo.build.pid=0x8036
leonardo.upload.protocol=avr109
leonardo.upload.speed=57600
",
        );
        let config = BoardConfig::from_boards_txt(f.path(), "leonardo", &HashMap::new()).unwrap();
        assert_eq!(config.vid, Some("0x2341".to_string()));
        assert_eq!(config.pid, Some("0x8036".to_string()));
        assert_eq!(config.upload_protocol, Some("avr109".to_string()));
        assert_eq!(config.upload_speed, Some("57600".to_string()));
    }

    #[test]
    fn test_get_defines_basic() {
        let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        let defines = config.get_defines();
        assert_eq!(defines.get("PLATFORMIO"), Some(&"1".to_string()));
        assert_eq!(defines.get("F_CPU"), Some(&"16000000L".to_string()));
        assert_eq!(defines.get("ARDUINO"), Some(&"10808".to_string()));
        assert_eq!(defines.get("ARDUINO_AVR_UNO"), Some(&"1".to_string()));
        assert_eq!(defines.get("ARDUINO_ARCH_AVR"), Some(&"1".to_string()));
        assert_eq!(defines.get("__AVR_ATMEGA328P__"), Some(&"1".to_string()));
    }

    #[test]
    fn test_get_defines_with_extra_flags() {
        let mut config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        config.extra_flags = Some("-DCUSTOM_FLAG -DVALUE=42".to_string());
        let defines = config.get_defines();
        assert_eq!(defines.get("CUSTOM_FLAG"), Some(&"1".to_string()));
        assert_eq!(defines.get("VALUE"), Some(&"42".to_string()));
    }

    #[test]
    fn test_get_include_paths() {
        let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        let paths = config.get_include_paths(Path::new("/framework"));
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], PathBuf::from("/framework/cores/arduino"));
        assert_eq!(paths[1], PathBuf::from("/framework/variants/standard"));
    }

    #[test]
    fn test_platform_detection() {
        let avr = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        assert_eq!(avr.platform(), Some(fbuild_core::Platform::AtmelAvr));

        let esp = BoardConfig::from_board_id("esp32dev", &HashMap::new()).unwrap();
        assert_eq!(esp.platform(), Some(fbuild_core::Platform::Espressif32));

        let teensy = BoardConfig::from_board_id("teensy41", &HashMap::new()).unwrap();
        assert_eq!(teensy.platform(), Some(fbuild_core::Platform::Teensy));

        let rpi = BoardConfig::from_board_id("rpipico", &HashMap::new()).unwrap();
        assert_eq!(rpi.platform(), Some(fbuild_core::Platform::RaspberryPi));
    }

    #[test]
    fn test_parse_boards_txt_with_comments() {
        let props = parse_boards_txt(
            "# This is a comment\nuno.name=Arduino Uno\n# Another comment\nuno.build.mcu=atmega328p\n",
            "uno",
        );
        assert_eq!(props.get("name"), Some(&"Arduino Uno".to_string()));
        assert_eq!(props.get("mcu"), Some(&"atmega328p".to_string()));
    }

    #[test]
    fn test_parse_boards_txt_empty_lines() {
        let props = parse_boards_txt(
            "\nuno.name=Arduino Uno\n\nuno.build.mcu=atmega328p\n\n",
            "uno",
        );
        assert_eq!(props.get("name"), Some(&"Arduino Uno".to_string()));
    }

    #[test]
    fn test_parse_boards_txt_ignores_other_boards() {
        let props = parse_boards_txt(
            "uno.name=Arduino Uno\nmega.name=Arduino Mega\nuno.build.mcu=atmega328p\n",
            "uno",
        );
        assert_eq!(props.get("name"), Some(&"Arduino Uno".to_string()));
        assert!(!props.values().any(|v| v == "Arduino Mega"));
    }

    #[test]
    fn test_repr() {
        let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        let repr = format!("{:?}", config);
        assert!(repr.contains("Arduino Uno"));
        assert!(repr.contains("atmega328p"));
    }

    // --- Data-driven enriched JSON tests ---

    #[test]
    fn test_mega_upload_protocol_wiring() {
        // Bug fix: mega needs protocol=wiring, not arduino
        let config = BoardConfig::from_board_id("mega", &HashMap::new()).unwrap();
        assert_eq!(config.upload_protocol, Some("wiring".to_string()));
        assert_eq!(config.core, "arduino");
        assert_eq!(config.variant, "mega");
    }

    #[test]
    fn test_nano_upload_speed_57600() {
        // Bug fix: nano uses 57600, not 115200
        let config = BoardConfig::from_board_id("nano", &HashMap::new()).unwrap();
        assert_eq!(config.upload_speed, Some("57600".to_string()));
        assert_eq!(config.upload_protocol, Some("arduino".to_string()));
        assert_eq!(config.variant, "eightanaloginputs");
    }

    #[test]
    fn test_teensy36_core_teensy3() {
        // Bug fix: teensy30-36 use core=teensy3, not teensy4
        let config = BoardConfig::from_board_id("teensy36", &HashMap::new()).unwrap();
        assert_eq!(config.core, "teensy3");
        assert_eq!(config.upload_protocol, Some("teensy-gui".to_string()));
    }

    #[test]
    fn test_teensy41_core_teensy4() {
        let config = BoardConfig::from_board_id("teensy41", &HashMap::new()).unwrap();
        assert_eq!(config.core, "teensy4");
        assert_eq!(config.upload_protocol, Some("teensy-gui".to_string()));
    }

    #[test]
    fn test_esp32dev_enriched_fields() {
        let config = BoardConfig::from_board_id("esp32dev", &HashMap::new()).unwrap();
        assert_eq!(config.core, "esp32");
        assert_eq!(config.variant, "esp32");
        // ESP32 boards now intentionally drop the JSON-shipped flash_mode
        // (see comments in `from_board_id`). Downstream consumers fall back
        // to mcu_config.default_flash_mode() which is "dio".
        assert_eq!(config.flash_mode, None);
        assert_eq!(config.memory_type, None);
        assert_eq!(config.f_flash, Some("40000000L".to_string()));
        assert_eq!(config.ldscript, Some("esp32_out.ld".to_string()));
        assert_eq!(config.upload_speed, Some("460800".to_string()));
    }

    #[test]
    fn test_esp32_flash_mode_env_override_honoured() {
        // The user can opt back into QIO via `board_build.flash_mode = qio`
        // in their [env:X] section, which the daemon translates into a
        // `flash_mode` override key.
        let mut overrides = HashMap::new();
        overrides.insert("flash_mode".to_string(), "qio".to_string());
        let config = BoardConfig::from_board_id("esp32dev", &overrides).unwrap();
        assert_eq!(config.flash_mode, Some("qio".to_string()));
    }

    #[test]
    fn test_pico_enriched_fields() {
        let config = BoardConfig::from_board_id("rpipico", &HashMap::new()).unwrap();
        assert_eq!(config.core, "earlephilhower");
        assert_eq!(config.variant, "rpipico");
        assert_eq!(config.upload_protocol, Some("picotool".to_string()));
    }

    #[test]
    fn test_extra_flags_produce_defines() {
        // Enriched extra_flags should produce correct ARDUINO_* defines
        let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        let defines = config.get_defines();
        assert_eq!(defines.get("ARDUINO_AVR_UNO"), Some(&"1".to_string()));
    }

    #[test]
    fn test_esp32c3_board_config() {
        let config = BoardConfig::from_board_id("esp32c3", &HashMap::new()).unwrap();
        assert_eq!(config.mcu, "esp32c3");
        assert_eq!(config.core, "esp32");
        // ESP32 boards drop the JSON-shipped flash_mode (see
        // `test_esp32dev_enriched_fields`); fall back is "dio" from MCU config.
        assert_eq!(config.flash_mode, None);
        assert_eq!(config.memory_type, None);
        assert_eq!(config.ldscript, Some("esp32c3_out.ld".to_string()));
        // ESP32-C3 DevKit runs at 160 MHz
        assert_eq!(config.f_cpu, "160000000L");
    }

    #[test]
    fn test_esp32_effective_memory_type_tracks_effective_flash_mode() {
        let config = BoardConfig::from_board_id("esp32c3", &HashMap::new()).unwrap();
        assert_eq!(
            config.effective_esp32_memory_type("dio"),
            Some("dio_qspi".to_string())
        );
    }

    #[test]
    fn test_esp32_effective_memory_type_preserves_opi_flash_profiles() {
        let config =
            BoardConfig::from_board_id("esp32-s3-devkitc-1-n32r8v", &HashMap::new()).unwrap();
        assert_eq!(
            config.effective_esp32_memory_type("dio"),
            Some("opi_opi".to_string())
        );
    }

    #[test]
    fn test_qemu_psram_config_absent_for_non_psram_board() {
        let config = BoardConfig::from_board_id("esp32-s3-devkitc-1", &HashMap::new()).unwrap();
        assert_eq!(config.qemu_esp32_psram_config(), None);
    }

    #[test]
    fn test_qemu_psram_config_detects_quad_psram_board() {
        let config = BoardConfig::from_board_id("esp32-s3-devkitc1-n8r2", &HashMap::new()).unwrap();
        assert_eq!(
            config.qemu_esp32_psram_config(),
            Some(Esp32QemuPsramConfig {
                size_mib: 2,
                is_octal: false,
            })
        );
    }

    #[test]
    fn test_qemu_psram_config_detects_octal_psram_board() {
        let config = BoardConfig::from_board_id("esp32-s3-devkitc1-n8r8", &HashMap::new()).unwrap();
        assert_eq!(
            config.qemu_esp32_psram_config(),
            Some(Esp32QemuPsramConfig {
                size_mib: 8,
                is_octal: true,
            })
        );
    }

    #[test]
    fn test_esp32c3_devkitm1_board_config() {
        // Direct look up by full board ID (same underlying JSON)
        let config = BoardConfig::from_board_id("esp32-c3-devkitm-1", &HashMap::new()).unwrap();
        assert_eq!(config.mcu, "esp32c3");
        let flags = config.extra_flags.unwrap_or_default();
        assert!(
            flags.contains("ARDUINO_ESP32C3_DEV"),
            "expected ARDUINO_ESP32C3_DEV in extra_flags, got: {flags}"
        );
    }

    #[test]
    fn test_esp32c3_no_psram() {
        // The plain C3 DevKit has no PSRAM
        let config = BoardConfig::from_board_id("esp32c3", &HashMap::new()).unwrap();
        let flags = config.extra_flags.clone().unwrap_or_default();
        assert!(
            !flags.contains("BOARD_HAS_PSRAM"),
            "ESP32-C3 DevKit should not have PSRAM flag, got: {flags}"
        );
    }

    // --- USB VID/PID defines ---

    #[test]
    fn test_get_defines_usb_vid_pid() {
        let mut config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        config.vid = Some("0x2341".to_string());
        config.pid = Some("0x8036".to_string());
        let defines = config.get_defines();
        assert_eq!(defines.get("USB_VID"), Some(&"0x2341".to_string()));
        assert_eq!(defines.get("USB_PID"), Some(&"0x8036".to_string()));
    }

    #[test]
    fn test_get_defines_no_usb_when_absent() {
        let config = BoardConfig::from_board_id("esp32dev", &HashMap::new()).unwrap();
        assert!(config.vid.is_none());
        let defines = config.get_defines();
        assert!(!defines.contains_key("USB_VID"));
        assert!(!defines.contains_key("USB_PID"));
    }

    #[test]
    fn test_leonardo_board_has_vid_pid() {
        let config = BoardConfig::from_board_id("leonardo", &HashMap::new()).unwrap();
        assert_eq!(config.vid, Some("0x2341".to_string()));
        assert_eq!(config.pid, Some("0x8036".to_string()));
        let defines = config.get_defines();
        assert_eq!(defines.get("USB_VID"), Some(&"0x2341".to_string()));
        assert_eq!(defines.get("USB_PID"), Some(&"0x8036".to_string()));
    }

    #[test]
    fn test_attiny1604_board_config() {
        let config = BoardConfig::from_board_id("ATtiny1604", &HashMap::new()).unwrap();
        assert_eq!(config.mcu, "attiny1604");
        assert_eq!(config.core, "megatinycore");
        assert_eq!(config.variant, "txy4");
        assert_eq!(config.platform(), Some(fbuild_core::Platform::AtmelMegaAvr));
        let defines = config.get_defines();
        assert_eq!(defines.get("ARDUINO_ARCH_MEGAAVR"), Some(&"1".to_string()));
    }

    #[test]
    fn test_nano_every_board_config() {
        let config = BoardConfig::from_board_id("nano_every", &HashMap::new()).unwrap();
        assert_eq!(config.mcu, "atmega4809");
        assert_eq!(config.core, "arduino");
        assert_eq!(config.variant, "nona4809");
        assert_eq!(config.platform(), Some(fbuild_core::Platform::AtmelMegaAvr));
    }

    /// Validate that ALL megatinycore boards have the required framework-injected
    /// defines in extra_flags. PlatformIO's builder script injects these at build
    /// time, but fbuild must carry them in the board JSON since we don't run
    /// PlatformIO's SCons scripts.
    #[test]
    fn test_megatinycore_boards_have_required_defines() {
        let db = get_board_db();
        let required = &[
            "MEGATINYCORE=",
            "MEGATINYCORE_MAJOR=",
            "MEGATINYCORE_MINOR=",
            "MEGATINYCORE_PATCH=",
            "MEGATINYCORE_RELEASED=",
            "CORE_ATTACH_ALL",
            "TWI_MORS",
            "CLOCK_SOURCE=",
        ];
        let mut failures = Vec::new();
        for (board_id, value) in db.iter() {
            let core = value
                .pointer("/build/core")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if core != "megatinycore" {
                continue;
            }
            let flags = value
                .pointer("/build/extra_flags")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            for &define in required {
                if !flags.contains(&format!("-D{define}")) {
                    failures.push(format!("{board_id}: missing -D{define}"));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "megatinycore boards missing required framework defines:\n{}",
            failures.join("\n")
        );
    }

    /// Validate that ALL dxcore boards have the required framework-injected
    /// defines in extra_flags.
    #[test]
    fn test_dxcore_boards_have_required_defines() {
        let db = get_board_db();
        let required = &[
            "DXCORE=",
            "DXCORE_MAJOR=",
            "DXCORE_MINOR=",
            "DXCORE_PATCH=",
            "DXCORE_RELEASED=",
            "CORE_ATTACH_ALL",
            "TWI_MORS_SINGLE",
            "MILLIS_USE_TIMER",
            "CLOCK_SOURCE=",
        ];
        let mut failures = Vec::new();
        for (board_id, value) in db.iter() {
            let core = value
                .pointer("/build/core")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if core != "dxcore" {
                continue;
            }
            let flags = value
                .pointer("/build/extra_flags")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            for &define in required {
                if !flags.contains(&format!("-D{define}")) {
                    failures.push(format!("{board_id}: missing -D{define}"));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "dxcore boards missing required framework defines:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn test_uno_debug_tools_has_simavr() {
        let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        let tools = config
            .debug_tools
            .as_ref()
            .expect("uno should have debug tools");
        assert!(tools.contains_key("simavr"), "uno should have simavr");
        assert!(tools.contains_key("avr-stub"), "uno should have avr-stub");
        // simavr is not marked as onboard in the board JSON
        assert!(!tools["simavr"].onboard);
    }

    #[test]
    fn test_emulators_filters_hardware_probes() {
        let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        let emus = config.emulators();
        // simavr is an emulator, avr-stub is not in EMULATOR_TOOL_NAMES
        assert!(emus.contains_key("simavr"), "simavr should be in emulators");
        assert!(
            !emus.contains_key("avr-stub"),
            "avr-stub is not an emulator"
        );
    }

    #[test]
    fn test_has_emulator() {
        let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        assert!(config.has_emulator("simavr"));
        assert!(!config.has_emulator("qemu"));
        assert!(!config.has_emulator("avr-stub")); // not in EMULATOR_TOOL_NAMES
    }

    #[test]
    fn test_debug_tools_round_trip_serde() {
        let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
        let json = serde_json::to_string(&config).unwrap();
        let restored: BoardConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.debug_tools, restored.debug_tools);
    }

    #[test]
    fn test_boards_txt_has_no_debug_tools() {
        let f = write_boards_txt(
            "\
uno.name=Arduino Uno
uno.build.mcu=atmega328p
uno.build.f_cpu=16000000L
",
        );
        let config = BoardConfig::from_boards_txt(f.path(), "uno", &HashMap::new()).unwrap();
        assert!(
            config.debug_tools.is_none(),
            "boards.txt should not have debug tools"
        );
    }

    #[test]
    fn test_esp32c6_devkitc1_has_variant() {
        // Regression: esp32-c6-devkitc-1.json was missing build.variant,
        // causing fbuild to look for variants/esp32-c6-devkitc-1/ instead
        // of variants/esp32c6/, which broke compilation (pins_arduino.h
        // not found). See https://github.com/FastLED/fbuild/issues/46
        let config = BoardConfig::from_board_id("esp32-c6-devkitc-1", &HashMap::new()).unwrap();
        assert_eq!(config.mcu, "esp32c6");
        assert_eq!(config.core, "esp32");
        assert_eq!(
            config.variant, "esp32c6",
            "esp32-c6-devkitc-1 must have variant=esp32c6, not the board ID fallback"
        );
    }

    #[test]
    fn test_esp32c6_alias_has_variant() {
        // The 'esp32c6' alias resolves to esp32-c6-devkitm-1 which has
        // the variant field. Verify both paths produce correct variant.
        let config = BoardConfig::from_board_id("esp32c6", &HashMap::new()).unwrap();
        assert_eq!(config.mcu, "esp32c6");
        assert_eq!(
            config.variant, "esp32c6",
            "esp32c6 alias must resolve to variant=esp32c6"
        );
    }

    #[test]
    fn test_board_without_debug_section() {
        // Find a board that has no debug section (if any), or verify graceful handling
        // by checking that debug_tools is populated from the JSON when present
        let config = BoardConfig::from_board_id("esp32dev", &HashMap::new()).unwrap();
        // esp32dev has debug tools (hardware probes only, no emulators)
        if let Some(ref tools) = config.debug_tools {
            let emus = config.emulators();
            // esp32dev has no software emulators, only hardware probes
            assert!(
                emus.is_empty(),
                "esp32dev should have no emulators, got: {:?}",
                emus
            );
            // But it should still have hardware debug tools
            assert!(!tools.is_empty(), "esp32dev should have debug tools");
        }
    }
}
