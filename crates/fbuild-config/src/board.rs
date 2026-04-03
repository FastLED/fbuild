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

/// Board configuration loaded from boards.txt or built-in defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardConfig {
    pub name: String,
    pub mcu: String,
    pub f_cpu: String,
    pub board: String,
    pub core: String,
    pub variant: String,
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
    /// Flash frequency (e.g. "80000000L") — ESP32 boards
    pub f_flash: Option<String>,
    /// Partition table file (e.g. "default_8MB.csv") — ESP32 boards
    pub partitions: Option<String>,
    /// Linker script (e.g. "esp32s3_out.ld")
    pub ldscript: Option<String>,
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

        Ok(Self {
            name,
            mcu,
            f_cpu: get("f_cpu").unwrap_or_else(|| "16000000L".to_string()),
            board: get("board")
                .or_else(|| props.get("board").cloned())
                .unwrap_or_else(|| board_id_to_board_define(board_id)),
            core: get("core").unwrap_or_else(|| "arduino".to_string()),
            variant: get("variant").unwrap_or_else(|| "standard".to_string()),
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
            flash_mode: get("flash_mode"),
            f_flash: get("f_flash"),
            partitions: get("partitions"),
            ldscript: get("ldscript"),
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

        Ok(Self {
            name: get("name", board_id),
            mcu: get("mcu", "unknown"),
            f_cpu: get("f_cpu", "16000000L"),
            board: get("board", &board_id_to_board_define(board_id)),
            core: get("core", "arduino"),
            variant: get("variant", "standard"),
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
            flash_mode: overrides
                .get("flash_mode")
                .cloned()
                .or_else(|| defaults.get("flash_mode").cloned()),
            f_flash: overrides
                .get("f_flash")
                .cloned()
                .or_else(|| defaults.get("f_flash").cloned()),
            partitions: overrides
                .get("partitions")
                .cloned()
                .or_else(|| defaults.get("partitions").cloned()),
            ldscript: overrides
                .get("ldscript")
                .cloned()
                .or_else(|| defaults.get("ldscript").cloned()),
        })
    }

    /// Detect the platform from the MCU name.
    pub fn platform(&self) -> Option<fbuild_core::Platform> {
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

        // Arduino version: Teensy boards use 10819, others use 10808
        let is_teensy = matches!(self.platform(), Some(fbuild_core::Platform::Teensy));
        let arduino_version = if is_teensy { "10819" } else { "10808" };
        defines.insert("ARDUINO".to_string(), arduino_version.to_string());

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

        // Teensy-specific defines
        if is_teensy {
            if mcu_upper.starts_with("IMXRT") {
                defines.insert(format!("__{}__", mcu_upper), "1".to_string());
            }
            defines.insert("TEENSYDUINO".to_string(), "159".to_string());
            defines.insert("USB_SERIAL".to_string(), "1".to_string());
            defines.insert("LAYOUT_US_ENGLISH".to_string(), "1".to_string());
        }

        // ESP32-specific defines
        let is_esp32 = matches!(self.platform(), Some(fbuild_core::Platform::Espressif32));
        if is_esp32 {
            defines.insert("ESP_PLATFORM".to_string(), "1".to_string());
            defines.insert("ESP32".to_string(), "ESP32".to_string());
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
            Some(fbuild_core::Platform::RaspberryPi) => "RP2040".to_string(),
            Some(fbuild_core::Platform::Ststm32) => "STM32".to_string(),
            Some(fbuild_core::Platform::Teensy) => "TEENSY".to_string(),
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
        "rpipico" => "pico",
        "rpipico2" => "pico2",
        "esp32c3" => "esp32-c3-devkitm-1",
        "esp32c6" => "esp32-c6-devkitm-1",
        "esp32s3" => "esp32-s3-devkitc-1",
        other => other,
    }
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

    // Data-driven: read build section from enriched JSON
    if let Some(build) = entry.get("build").and_then(|v| v.as_object()) {
        if let Some(core) = build.get("core").and_then(|v| v.as_str()) {
            d.insert("core".into(), core.to_string());
        }
        if let Some(variant) = build.get("variant").and_then(|v| v.as_str()) {
            d.insert("variant".into(), variant.to_string());
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
        if let Some(f_flash) = build.get("f_flash").and_then(|v| v.as_str()) {
            d.insert("f_flash".into(), f_flash.to_string());
        }
        // Arduino sub-fields
        if let Some(arduino) = build.get("arduino").and_then(|v| v.as_object()) {
            if let Some(ldscript) = arduino.get("ldscript").and_then(|v| v.as_str()) {
                d.insert("ldscript".into(), ldscript.to_string());
            }
            if let Some(partitions) = arduino.get("partitions").and_then(|v| v.as_str()) {
                d.insert("partitions".into(), partitions.to_string());
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
        d.insert("variant".into(), board_id.to_string());
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
        assert_eq!(config.flash_mode, Some("dio".to_string()));
        assert_eq!(config.f_flash, Some("40000000L".to_string()));
        assert_eq!(config.ldscript, Some("esp32_out.ld".to_string()));
        assert_eq!(config.upload_speed, Some("460800".to_string()));
    }

    #[test]
    fn test_pico_enriched_fields() {
        let config = BoardConfig::from_board_id("rpipico", &HashMap::new()).unwrap();
        assert_eq!(config.core, "arduino");
        assert_eq!(config.variant, "RASPBERRY_PI_PICO");
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
        assert_eq!(config.flash_mode, Some("qio".to_string()));
        assert_eq!(config.ldscript, Some("esp32c3_out.ld".to_string()));
        // ESP32-C3 DevKit runs at 160 MHz
        assert_eq!(config.f_cpu, "160000000L");
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
        let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
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
}
