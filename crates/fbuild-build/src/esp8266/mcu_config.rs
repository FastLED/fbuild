//! Data-driven ESP8266 MCU configuration from embedded JSON.
//!
//! The ESP8266 has a single MCU variant (Xtensa LX106), so there is one
//! embedded config file — much simpler than the ESP32 family.

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

use crate::compiler::{CompilerFlags, McuConfig, ObjcopyConfig, ProfileFlags};

const ESP8266_JSON: &str = include_str!("configs/esp8266.json");

/// Esptool configuration for ESP8266.
#[derive(Debug, Clone, Deserialize)]
pub struct Esp8266EsptoolConfig {
    pub chip: String,
    pub default_flash_mode: String,
    pub default_flash_freq: String,
    pub default_flash_size: String,
    pub default_baud: u32,
}

/// Complete ESP8266 MCU configuration parsed from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct Esp8266McuConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub architecture: String,
    pub compiler_flags: CompilerFlags,
    pub linker_flags: Vec<String>,
    pub linker_libs: Vec<String>,
    pub objcopy: ObjcopyConfig,
    pub profiles: HashMap<String, ProfileFlags>,
    pub esptool: Esp8266EsptoolConfig,
}

impl McuConfig for Esp8266McuConfig {
    fn compiler_flags(&self) -> &CompilerFlags {
        &self.compiler_flags
    }

    fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

/// Load the ESP8266 MCU configuration.
pub fn get_esp8266_config() -> Result<Esp8266McuConfig> {
    serde_json::from_str(ESP8266_JSON).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!("failed to parse ESP8266 MCU config: {}", e))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_esp8266_config_parses() {
        let config = get_esp8266_config().unwrap();
        assert_eq!(config.name, "ESP8266");
        assert_eq!(config.architecture, "xtensa-lx106");
    }

    #[test]
    fn test_compiler_flags_non_empty() {
        let config = get_esp8266_config().unwrap();
        assert!(!config.compiler_flags.common.is_empty());
        assert!(!config.compiler_flags.c.is_empty());
        assert!(!config.compiler_flags.cxx.is_empty());
    }

    #[test]
    fn test_compiler_flags_content() {
        let config = get_esp8266_config().unwrap();
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mlongcalls".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mtext-section-literals".to_string()));
        assert!(config.compiler_flags.c.contains(&"-std=gnu17".to_string()));
        assert!(config
            .compiler_flags
            .cxx
            .contains(&"-std=gnu++17".to_string()));
        assert!(config.compiler_flags.cxx.contains(&"-fno-rtti".to_string()));
    }

    #[test]
    fn test_linker_flags() {
        let config = get_esp8266_config().unwrap();
        assert!(config.linker_flags.contains(&"-nostdlib".to_string()));
        assert!(config
            .linker_flags
            .contains(&"-Wl,--gc-sections".to_string()));
    }

    #[test]
    fn test_linker_libs() {
        let config = get_esp8266_config().unwrap();
        assert!(config.linker_libs.contains(&"-lm".to_string()));
        assert!(config.linker_libs.contains(&"-lgcc".to_string()));
        assert!(config.linker_libs.contains(&"-lmain".to_string()));
    }

    #[test]
    fn test_profiles() {
        let config = get_esp8266_config().unwrap();
        let release = config.profiles.get("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));

        let quick = config.profiles.get("quick").unwrap();
        assert!(quick.compile_flags.contains(&"-Os".to_string()));
    }

    #[test]
    fn test_esptool_config() {
        let config = get_esp8266_config().unwrap();
        assert_eq!(config.esptool.chip, "esp8266");
        assert_eq!(config.esptool.default_flash_mode, "dio");
        assert_eq!(config.esptool.default_flash_freq, "40m");
        assert_eq!(config.esptool.default_flash_size, "4MB");
        assert_eq!(config.esptool.default_baud, 115200);
    }
}
