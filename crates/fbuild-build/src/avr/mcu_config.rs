//! Data-driven AVR MCU configuration from embedded JSON.
//!
//! AVR ATmega variants share compiler/linker flags — the MCU-specific `-mmcu=`
//! flag is prepended at runtime from `BoardConfig.mcu`.

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

const AVR_JSON: &str = include_str!("configs/avr.json");

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

/// Objcopy configuration for firmware conversion.
#[derive(Debug, Clone, Deserialize)]
pub struct ObjcopyConfig {
    pub output_format: String,
    pub remove_sections: Vec<String>,
}

/// Avrdude deploy tool configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct AvrdudeConfig {
    pub default_programmer: String,
    pub default_baud: u32,
    pub timeout_secs: u64,
}

/// Complete AVR MCU configuration parsed from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct AvrMcuConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub architecture: String,
    pub compiler_flags: CompilerFlags,
    pub linker_flags: Vec<String>,
    pub linker_libs: Vec<String>,
    pub objcopy: ObjcopyConfig,
    pub profiles: HashMap<String, ProfileFlags>,
    pub avrdude: AvrdudeConfig,
}

impl AvrMcuConfig {
    /// Get profile flags for a given profile name.
    pub fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

/// Load the AVR MCU configuration.
pub fn get_avr_config() -> Result<AvrMcuConfig> {
    serde_json::from_str(AVR_JSON).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!("failed to parse AVR MCU config: {}", e))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_avr_config_parses() {
        let config = get_avr_config().unwrap();
        assert_eq!(config.name, "AVR ATmega");
        assert_eq!(config.architecture, "avr");
    }

    #[test]
    fn test_compiler_flags_non_empty() {
        let config = get_avr_config().unwrap();
        assert!(!config.compiler_flags.common.is_empty());
        assert!(!config.compiler_flags.c.is_empty());
        assert!(!config.compiler_flags.cxx.is_empty());
    }

    #[test]
    fn test_compiler_flags_content() {
        let config = get_avr_config().unwrap();
        // Optimization flags (-Os, -flto) are in profiles, not common
        assert!(config
            .compiler_flags
            .common
            .contains(&"-ffunction-sections".to_string()));
        assert!(config.compiler_flags.c.contains(&"-std=gnu11".to_string()));
        assert!(config
            .compiler_flags
            .cxx
            .contains(&"-std=gnu++11".to_string()));
        assert!(config
            .compiler_flags
            .cxx
            .contains(&"-fno-exceptions".to_string()));
    }

    #[test]
    fn test_linker_flags() {
        let config = get_avr_config().unwrap();
        // Optimization flags (-Os, -flto) are in profiles, not linker_flags
        assert!(config
            .linker_flags
            .contains(&"-Wl,--gc-sections".to_string()));
    }

    #[test]
    fn test_linker_libs() {
        let config = get_avr_config().unwrap();
        assert!(config.linker_libs.contains(&"-lm".to_string()));
    }

    #[test]
    fn test_objcopy_config() {
        let config = get_avr_config().unwrap();
        assert_eq!(config.objcopy.output_format, "ihex");
        assert!(config
            .objcopy
            .remove_sections
            .contains(&".eeprom".to_string()));
    }

    #[test]
    fn test_profiles() {
        let config = get_avr_config().unwrap();
        let release = config.get_profile("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));
        assert!(release.compile_flags.contains(&"-flto".to_string()));

        let quick = config.get_profile("quick").unwrap();
        assert!(quick.compile_flags.contains(&"-Os".to_string()));
        assert!(quick.link_flags.is_empty());
    }

    #[test]
    fn test_avrdude_config() {
        let config = get_avr_config().unwrap();
        assert_eq!(config.avrdude.default_programmer, "arduino");
        assert_eq!(config.avrdude.default_baud, 115200);
        assert_eq!(config.avrdude.timeout_secs, 60);
    }
}
