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
    /// Returns defines like: PLATFORMIO, F_CPU, ARDUINO, ARDUINO_<BOARD>, ARDUINO_ARCH_<ARCH>
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

        // Extra flags
        if let Some(ref flags) = self.extra_flags {
            for flag in flags.split_whitespace() {
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

/// Convert a board_id like "uno" to a board define like "AVR_UNO".
fn board_id_to_board_define(board_id: &str) -> String {
    board_id.to_uppercase().replace('-', "_")
}

/// Built-in defaults for common boards.
fn get_board_defaults(board_id: &str) -> Option<HashMap<String, String>> {
    let mut d = HashMap::new();

    match board_id {
        "uno" => {
            d.insert("name".into(), "Arduino Uno".into());
            d.insert("mcu".into(), "atmega328p".into());
            d.insert("f_cpu".into(), "16000000L".into());
            d.insert("board".into(), "AVR_UNO".into());
            d.insert("core".into(), "arduino".into());
            d.insert("variant".into(), "standard".into());
            d.insert("maximum_size".into(), "32256".into());
            d.insert("maximum_data_size".into(), "2048".into());
            d.insert("upload.protocol".into(), "arduino".into());
            d.insert("upload.speed".into(), "115200".into());
        }
        "mega" | "megaatmega2560" => {
            d.insert("name".into(), "Arduino Mega 2560".into());
            d.insert("mcu".into(), "atmega2560".into());
            d.insert("f_cpu".into(), "16000000L".into());
            d.insert("board".into(), "AVR_MEGA2560".into());
            d.insert("core".into(), "arduino".into());
            d.insert("variant".into(), "mega".into());
            d.insert("maximum_size".into(), "253952".into());
            d.insert("maximum_data_size".into(), "8192".into());
            d.insert("upload.protocol".into(), "wiring".into());
            d.insert("upload.speed".into(), "115200".into());
        }
        "nano" | "nanoatmega328" => {
            d.insert("name".into(), "Arduino Nano".into());
            d.insert("mcu".into(), "atmega328p".into());
            d.insert("f_cpu".into(), "16000000L".into());
            d.insert("board".into(), "AVR_NANO".into());
            d.insert("core".into(), "arduino".into());
            d.insert("variant".into(), "eightanaloginputs".into());
            d.insert("maximum_size".into(), "30720".into());
            d.insert("maximum_data_size".into(), "2048".into());
            d.insert("upload.protocol".into(), "arduino".into());
            d.insert("upload.speed".into(), "57600".into());
        }
        "leonardo" => {
            d.insert("name".into(), "Arduino Leonardo".into());
            d.insert("mcu".into(), "atmega32u4".into());
            d.insert("f_cpu".into(), "16000000L".into());
            d.insert("board".into(), "AVR_LEONARDO".into());
            d.insert("core".into(), "arduino".into());
            d.insert("variant".into(), "leonardo".into());
            d.insert("vid".into(), "0x2341".into());
            d.insert("pid".into(), "0x8036".into());
            d.insert("maximum_size".into(), "28672".into());
            d.insert("maximum_data_size".into(), "2560".into());
            d.insert("upload.protocol".into(), "avr109".into());
            d.insert("upload.speed".into(), "57600".into());
        }
        "esp32dev" => {
            d.insert("name".into(), "Espressif ESP32 Dev Module".into());
            d.insert("mcu".into(), "esp32".into());
            d.insert("f_cpu".into(), "240000000L".into());
            d.insert("board".into(), "ESP32_DEV".into());
            d.insert("core".into(), "esp32".into());
            d.insert("variant".into(), "esp32".into());
            d.insert("maximum_size".into(), "1310720".into());
            d.insert("maximum_data_size".into(), "327680".into());
            d.insert("upload.protocol".into(), "esptool".into());
            d.insert("upload.speed".into(), "460800".into());
        }
        "esp32-c3" | "esp32c3" => {
            d.insert("name".into(), "ESP32-C3".into());
            d.insert("mcu".into(), "esp32c3".into());
            d.insert("f_cpu".into(), "160000000L".into());
            d.insert("board".into(), "ESP32C3_DEV".into());
            d.insert("core".into(), "esp32".into());
            d.insert("variant".into(), "esp32c3".into());
            d.insert("maximum_size".into(), "3145728".into());
            d.insert("maximum_data_size".into(), "327680".into());
        }
        "esp32-c6" | "esp32c6" => {
            d.insert("name".into(), "ESP32-C6".into());
            d.insert("mcu".into(), "esp32c6".into());
            d.insert("f_cpu".into(), "160000000L".into());
            d.insert("board".into(), "ESP32C6_DEV".into());
            d.insert("core".into(), "esp32".into());
            d.insert("variant".into(), "esp32c6".into());
            d.insert("maximum_size".into(), "3145728".into());
            d.insert("maximum_data_size".into(), "327680".into());
        }
        "esp32-s3" | "esp32s3" => {
            d.insert("name".into(), "ESP32-S3".into());
            d.insert("mcu".into(), "esp32s3".into());
            d.insert("f_cpu".into(), "240000000L".into());
            d.insert("board".into(), "ESP32S3_DEV".into());
            d.insert("core".into(), "esp32".into());
            d.insert("variant".into(), "esp32s3".into());
            d.insert("maximum_size".into(), "3145728".into());
            d.insert("maximum_data_size".into(), "327680".into());
        }
        "teensy40" => {
            d.insert("name".into(), "Teensy 4.0".into());
            d.insert("mcu".into(), "imxrt1062".into());
            d.insert("f_cpu".into(), "600000000L".into());
            d.insert("board".into(), "TEENSY40".into());
            d.insert("core".into(), "teensy4".into());
            d.insert("variant".into(), "teensy40".into());
            d.insert("maximum_size".into(), "2031616".into());
            d.insert("maximum_data_size".into(), "1048576".into());
            d.insert("upload.protocol".into(), "teensy-gui".into());
            d.insert("upload.speed".into(), "0".into());
        }
        "teensy41" => {
            d.insert("name".into(), "Teensy 4.1".into());
            d.insert("mcu".into(), "imxrt1062".into());
            d.insert("f_cpu".into(), "600000000L".into());
            d.insert("board".into(), "TEENSY41".into());
            d.insert("core".into(), "teensy4".into());
            d.insert("variant".into(), "teensy41".into());
            d.insert("maximum_size".into(), "8126464".into());
            d.insert("maximum_data_size".into(), "1048576".into());
            d.insert("upload.protocol".into(), "teensy-gui".into());
            d.insert("upload.speed".into(), "0".into());
        }
        "rpipico" | "pico" => {
            d.insert("name".into(), "Raspberry Pi Pico".into());
            d.insert("mcu".into(), "rp2040".into());
            d.insert("f_cpu".into(), "133000000L".into());
            d.insert("board".into(), "RASPBERRY_PI_PICO".into());
            d.insert("core".into(), "arduino".into());
            d.insert("variant".into(), "rpipico".into());
            d.insert("maximum_size".into(), "2097152".into());
            d.insert("maximum_data_size".into(), "262144".into());
        }
        "rpipico2" | "pico2" => {
            d.insert("name".into(), "Raspberry Pi Pico 2".into());
            d.insert("mcu".into(), "rp2350".into());
            d.insert("f_cpu".into(), "150000000L".into());
            d.insert("board".into(), "RASPBERRY_PI_PICO2".into());
            d.insert("core".into(), "arduino".into());
            d.insert("variant".into(), "rpipico2".into());
            d.insert("maximum_size".into(), "4194304".into());
            d.insert("maximum_data_size".into(), "524288".into());
        }
        "bluepill_f103c8" => {
            d.insert("name".into(), "STM32 Blue Pill F103C8".into());
            d.insert("mcu".into(), "stm32f103c8t6".into());
            d.insert("f_cpu".into(), "72000000L".into());
            d.insert("board".into(), "BLUEPILL_F103C8".into());
            d.insert("core".into(), "stm32".into());
            d.insert("variant".into(), "STM32F1xx/F103C8T_F103CB(T-U)".into());
            d.insert("maximum_size".into(), "65536".into());
            d.insert("maximum_data_size".into(), "20480".into());
        }
        "nucleo_f446re" => {
            d.insert("name".into(), "ST Nucleo F446RE".into());
            d.insert("mcu".into(), "stm32f446ret6".into());
            d.insert("f_cpu".into(), "180000000L".into());
            d.insert("board".into(), "NUCLEO_F446RE".into());
            d.insert("core".into(), "stm32".into());
            d.insert("variant".into(), "STM32F4xx/F446R(C-E)T".into());
            d.insert("maximum_size".into(), "524288".into());
            d.insert("maximum_data_size".into(), "131072".into());
        }
        _ => return None,
    }

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
        assert_eq!(config.board, "AVR_UNO");
        assert_eq!(config.core, "arduino");
        assert_eq!(config.variant, "standard");
        assert_eq!(config.max_flash, Some(32256));
        assert_eq!(config.max_ram, Some(2048));
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
}
