//! Data-driven ESP32 MCU configuration from embedded JSON files.
//!
//! Each ESP32 variant has a JSON config (from the Python reference) embedded at compile time.
//! The config drives all compiler flags, linker flags, linker scripts, defines, and esptool
//! flash offsets — no hardcoded per-variant flags elsewhere.

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

// Embed JSON configs at compile time.
const ESP32_JSON: &str = include_str!("configs/esp32.json");
const ESP32C2_JSON: &str = include_str!("configs/esp32c2.json");
const ESP32C3_JSON: &str = include_str!("configs/esp32c3.json");
const ESP32C5_JSON: &str = include_str!("configs/esp32c5.json");
const ESP32C6_JSON: &str = include_str!("configs/esp32c6.json");
const ESP32P4_JSON: &str = include_str!("configs/esp32p4.json");
const ESP32S3_JSON: &str = include_str!("configs/esp32s3.json");

/// Compiler flags split by language.
#[derive(Debug, Clone, Deserialize)]
pub struct CompilerFlags {
    pub common: Vec<String>,
    pub c: Vec<String>,
    pub cxx: Vec<String>,
}

/// Profile-specific build flags (release, quick).
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileFlags {
    pub compile_flags: Vec<String>,
    pub link_flags: Vec<String>,
}

/// Esptool flash configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct EsptoolConfig {
    pub elf_sha256_offset: String,
    pub flash_offsets: FlashOffsets,
    /// Default flash mode for esptool (e.g. "dio", "qio").
    pub default_flash_mode: String,
    /// Default flash frequency for esptool (e.g. "80m", "40m").
    pub default_flash_freq: String,
    /// Default flash size for esptool (e.g. "4MB").
    pub default_flash_size: String,
    /// Default baud rate for flashing.
    pub default_baud: u32,
    /// Reset mode before flashing (e.g. "default_reset").
    pub before_reset: String,
    /// Reset mode after flashing (e.g. "hard_reset").
    pub after_reset: String,
}

/// Flash memory offsets for bootloader, partitions, and firmware.
#[derive(Debug, Clone, Deserialize)]
pub struct FlashOffsets {
    pub bootloader: String,
    pub partitions: String,
    pub firmware: String,
}

/// A define entry: either a simple string or a [key, value] pair.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum DefineEntry {
    Simple(String),
    KeyValue(String, String),
}

/// Complete MCU configuration parsed from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct Esp32McuConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub mcu: String,
    pub architecture: String,
    pub compiler_flags: CompilerFlags,
    pub linker_flags: Vec<String>,
    pub linker_scripts: Vec<String>,
    pub linker_libs: Vec<String>,
    pub profiles: HashMap<String, ProfileFlags>,
    pub esptool: EsptoolConfig,
    pub defines: Vec<DefineEntry>,
    /// Compatibility defines (e.g. mbedtls `_ret` suffix renames).
    /// Each entry is a [old_name, new_name] pair emitted as `-Dold_name=new_name`.
    #[serde(default)]
    pub compat_defines: Vec<(String, String)>,
}

impl Esp32McuConfig {
    /// Whether this MCU uses RISC-V architecture.
    pub fn is_riscv(&self) -> bool {
        self.architecture.starts_with("riscv")
    }

    /// Whether this MCU uses Xtensa architecture.
    pub fn is_xtensa(&self) -> bool {
        self.architecture.starts_with("xtensa")
    }

    /// Get the toolchain binary prefix for this MCU.
    ///
    /// For Xtensa MCUs, uses the MCU-specific wrapper prefix (e.g., `xtensa-esp32-elf-`)
    /// because the generic `xtensa-esp-elf-` defaults to big-endian. The MCU-specific
    /// wrappers automatically add `-mdynconfig` for correct endianness.
    pub fn toolchain_prefix(&self) -> String {
        if self.is_riscv() {
            "riscv32-esp-elf-".to_string()
        } else {
            format!("xtensa-{}-elf-", self.mcu)
        }
    }

    /// Convert defines to a HashMap suitable for CompilerBase.
    pub fn defines_map(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for entry in &self.defines {
            match entry {
                DefineEntry::Simple(name) => {
                    map.insert(name.clone(), "1".to_string());
                }
                DefineEntry::KeyValue(name, value) => {
                    map.insert(name.clone(), value.clone());
                }
            }
        }
        map
    }

    /// Get the bootloader flash offset as a hex string.
    pub fn bootloader_offset(&self) -> &str {
        &self.esptool.flash_offsets.bootloader
    }

    /// Get the firmware flash offset as a hex string.
    pub fn firmware_offset(&self) -> &str {
        &self.esptool.flash_offsets.firmware
    }

    /// Get the partitions flash offset as a hex string.
    pub fn partitions_offset(&self) -> &str {
        &self.esptool.flash_offsets.partitions
    }

    /// Default flash mode (e.g. "dio").
    pub fn default_flash_mode(&self) -> &str {
        &self.esptool.default_flash_mode
    }

    /// Default flash frequency (e.g. "80m").
    pub fn default_flash_freq(&self) -> &str {
        &self.esptool.default_flash_freq
    }

    /// Default flash size (e.g. "4MB").
    pub fn default_flash_size(&self) -> &str {
        &self.esptool.default_flash_size
    }

    /// Default baud rate for flashing.
    pub fn default_baud(&self) -> u32 {
        self.esptool.default_baud
    }

    /// Reset mode before flashing (e.g. "default_reset").
    pub fn before_reset(&self) -> &str {
        &self.esptool.before_reset
    }

    /// Reset mode after flashing (e.g. "hard_reset").
    pub fn after_reset(&self) -> &str {
        &self.esptool.after_reset
    }

    /// Get profile flags for a given profile name.
    pub fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }

    /// Produce `-Dold=new` flags from the `compat_defines` entries.
    pub fn compat_define_flags(&self) -> Vec<String> {
        self.compat_defines
            .iter()
            .map(|(old, new)| format!("-D{}={}", old, new))
            .collect()
    }

    /// Remove LTO-related flags from all profiles.
    ///
    /// Called when the SDK specifies `-fno-lto` in its linker flags, meaning
    /// objects must not be compiled with LTO.
    pub fn disable_lto(&mut self) {
        for profile in self.profiles.values_mut() {
            profile
                .compile_flags
                .retain(|f| !f.contains("lto") && f != "-fuse-linker-plugin");
            profile
                .link_flags
                .retain(|f| !f.contains("lto") && f != "-fuse-linker-plugin");
        }
    }
}

/// Convert a flash size in bytes to an esptool-compatible size string (e.g. "4MB").
///
/// Falls back to `default` for unrecognized sizes.
pub fn bytes_to_flash_size(bytes: Option<u64>, default: &str) -> &str {
    match bytes {
        Some(sz) if sz >= 128 * 1024 * 1024 => "128MB",
        Some(sz) if sz >= 64 * 1024 * 1024 => "64MB",
        Some(sz) if sz >= 32 * 1024 * 1024 => "32MB",
        Some(sz) if sz >= 16 * 1024 * 1024 => "16MB",
        Some(sz) if sz >= 8 * 1024 * 1024 => "8MB",
        Some(sz) if sz >= 4 * 1024 * 1024 => "4MB",
        Some(sz) if sz >= 2 * 1024 * 1024 => "2MB",
        Some(sz) if sz >= 1024 * 1024 => "1MB",
        _ => default,
    }
}

/// Load the MCU configuration for a given MCU name.
///
/// Supported MCUs: esp32, esp32c2, esp32c3, esp32c5, esp32c6, esp32p4, esp32s3
pub fn get_mcu_config(mcu: &str) -> Result<Esp32McuConfig> {
    let json = match mcu {
        "esp32" => ESP32_JSON,
        "esp32c2" => ESP32C2_JSON,
        "esp32c3" => ESP32C3_JSON,
        "esp32c5" => ESP32C5_JSON,
        "esp32c6" => ESP32C6_JSON,
        "esp32p4" => ESP32P4_JSON,
        "esp32s3" => ESP32S3_JSON,
        _ => {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "unsupported ESP32 MCU: '{}' (supported: esp32, esp32c2, esp32c3, esp32c5, esp32c6, esp32p4, esp32s3)",
                mcu
            )));
        }
    };

    serde_json::from_str(json).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse MCU config for '{}': {}",
            mcu, e
        ))
    })
}

/// List all supported ESP32 MCU names.
pub fn supported_mcus() -> &'static [&'static str] {
    &[
        "esp32", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32p4", "esp32s3",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_configs_parse() {
        for mcu in supported_mcus() {
            let config = get_mcu_config(mcu).unwrap_or_else(|e| {
                panic!("failed to parse config for {}: {}", mcu, e);
            });
            assert_eq!(config.mcu, *mcu);
            assert!(!config.compiler_flags.common.is_empty());
            assert!(!config.linker_flags.is_empty());
            assert!(!config.linker_scripts.is_empty());
            assert!(!config.linker_libs.is_empty());
            assert!(!config.defines.is_empty());
        }
    }

    #[test]
    fn test_architecture_detection() {
        let esp32 = get_mcu_config("esp32").unwrap();
        assert!(esp32.is_xtensa());
        assert!(!esp32.is_riscv());

        let esp32s3 = get_mcu_config("esp32s3").unwrap();
        assert!(esp32s3.is_xtensa());

        let esp32c6 = get_mcu_config("esp32c6").unwrap();
        assert!(esp32c6.is_riscv());
        assert!(!esp32c6.is_xtensa());

        let esp32c3 = get_mcu_config("esp32c3").unwrap();
        assert!(esp32c3.is_riscv());
    }

    #[test]
    fn test_toolchain_prefix() {
        let esp32 = get_mcu_config("esp32").unwrap();
        assert_eq!(esp32.toolchain_prefix(), "xtensa-esp32-elf-");

        let esp32c6 = get_mcu_config("esp32c6").unwrap();
        assert_eq!(esp32c6.toolchain_prefix(), "riscv32-esp-elf-");
    }

    #[test]
    fn test_bootloader_offsets() {
        // ESP32 uses 0x1000 for bootloader
        let esp32 = get_mcu_config("esp32").unwrap();
        assert_eq!(esp32.bootloader_offset(), "0x1000");

        // C-series and S3 use 0x0
        let esp32c6 = get_mcu_config("esp32c6").unwrap();
        assert_eq!(esp32c6.bootloader_offset(), "0x0");

        let esp32s3 = get_mcu_config("esp32s3").unwrap();
        assert_eq!(esp32s3.bootloader_offset(), "0x0");
    }

    #[test]
    fn test_firmware_offset_consistent() {
        for mcu in supported_mcus() {
            let config = get_mcu_config(mcu).unwrap();
            assert_eq!(config.firmware_offset(), "0x10000");
            assert_eq!(config.partitions_offset(), "0x8000");
        }
    }

    #[test]
    fn test_defines_map() {
        let config = get_mcu_config("esp32c6").unwrap();
        let defines = config.defines_map();
        assert_eq!(defines.get("ESP_PLATFORM"), Some(&"1".to_string()));
        assert_eq!(defines.get("ARDUINO_ARCH_ESP32"), Some(&"1".to_string()));
        // Key-value defines
        assert!(defines.contains_key("IDF_VER"));
        assert!(defines.get("IDF_VER").unwrap().contains("v5."));
    }

    #[test]
    fn test_profiles() {
        let config = get_mcu_config("esp32c6").unwrap();
        let release = config.get_profile("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));
        assert!(release.compile_flags.contains(&"-flto=auto".to_string()));

        let quick = config.get_profile("quick").unwrap();
        assert!(quick.compile_flags.contains(&"-Os".to_string()));
        assert!(quick.link_flags.is_empty());
    }

    #[test]
    fn test_linker_scripts_per_mcu() {
        let esp32 = get_mcu_config("esp32").unwrap();
        assert!(esp32.linker_scripts.iter().any(|s| s.contains("esp32.rom")));
        assert!(esp32.linker_scripts.contains(&"memory.ld".to_string()));
        assert!(esp32.linker_scripts.contains(&"sections.ld".to_string()));

        let esp32c6 = get_mcu_config("esp32c6").unwrap();
        assert!(esp32c6
            .linker_scripts
            .iter()
            .any(|s| s.contains("esp32c6.rom")));
    }

    #[test]
    fn test_unsupported_mcu() {
        let result = get_mcu_config("esp32h2");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported"));
    }

    #[test]
    fn test_esp32_xtensa_has_mlongcalls() {
        let esp32 = get_mcu_config("esp32").unwrap();
        assert!(esp32
            .compiler_flags
            .common
            .contains(&"-mlongcalls".to_string()));

        let esp32s3 = get_mcu_config("esp32s3").unwrap();
        assert!(esp32s3
            .compiler_flags
            .common
            .contains(&"-mlongcalls".to_string()));
    }

    #[test]
    fn test_riscv_has_march() {
        let esp32c6 = get_mcu_config("esp32c6").unwrap();
        assert!(esp32c6
            .compiler_flags
            .c
            .iter()
            .any(|f| f.starts_with("-march=rv32")));

        let esp32p4 = get_mcu_config("esp32p4").unwrap();
        assert!(esp32p4
            .compiler_flags
            .c
            .iter()
            .any(|f| f.contains("rv32imafc")));
        assert!(esp32p4
            .compiler_flags
            .c
            .iter()
            .any(|f| f.contains("ilp32f")));
    }

    #[test]
    fn test_linker_flag_counts() {
        // ESP32 MCUs have 40+ linker flags (including -u symbols)
        for mcu in supported_mcus() {
            let config = get_mcu_config(mcu).unwrap();
            assert!(
                config.linker_flags.len() >= 20,
                "{} has only {} linker flags",
                mcu,
                config.linker_flags.len()
            );
        }
    }

    #[test]
    fn test_compat_defines_present() {
        for mcu in supported_mcus() {
            let config = get_mcu_config(mcu).unwrap();
            assert!(
                !config.compat_defines.is_empty(),
                "{} missing compat_defines",
                mcu
            );
        }
    }

    #[test]
    fn test_compat_define_flags() {
        let config = get_mcu_config("esp32c6").unwrap();
        let flags = config.compat_define_flags();
        assert!(flags.contains(&"-Dmbedtls_md5_starts_ret=mbedtls_md5_starts".to_string()));
        assert!(flags.contains(&"-Dmbedtls_sha1_finish_ret=mbedtls_sha1_finish".to_string()));
        assert_eq!(flags.len(), 6);
    }
}
