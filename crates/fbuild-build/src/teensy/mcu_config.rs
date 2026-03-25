//! Data-driven Teensy MCU configuration from embedded JSON.
//!
//! Teensy 4.x boards share ARM Cortex-M7 compiler/linker flags. Board-specific
//! details (linker script, memory limits) come from `BoardConfig` at runtime.

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

const TEENSY4X_JSON: &str = include_str!("configs/teensy4x.json");

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

/// Teensy loader CLI deploy tool configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct TeensyLoaderConfig {
    pub wait_flag: String,
    pub verbose_flag: String,
    pub timeout_secs: u64,
}

/// Complete Teensy MCU configuration parsed from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct TeensyMcuConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub architecture: String,
    pub compiler_flags: CompilerFlags,
    pub linker_flags: Vec<String>,
    pub linker_libs: Vec<String>,
    pub objcopy: ObjcopyConfig,
    pub profiles: HashMap<String, ProfileFlags>,
    pub teensy_loader: TeensyLoaderConfig,
}

impl TeensyMcuConfig {
    /// Get profile flags for a given profile name.
    pub fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

/// Load the Teensy 4.x MCU configuration.
pub fn get_teensy_config() -> Result<TeensyMcuConfig> {
    serde_json::from_str(TEENSY4X_JSON).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!("failed to parse Teensy MCU config: {}", e))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_teensy_config_parses() {
        let config = get_teensy_config().unwrap();
        assert_eq!(config.name, "Teensy 4.x");
        assert_eq!(config.architecture, "arm-cortex-m7");
    }

    #[test]
    fn test_compiler_flags_content() {
        let config = get_teensy_config().unwrap();
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mcpu=cortex-m7".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mthumb".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mfloat-abi=hard".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mfpu=fpv5-d16".to_string()));
        assert!(config.compiler_flags.c.contains(&"-std=gnu11".to_string()));
        assert!(config
            .compiler_flags
            .cxx
            .contains(&"-std=gnu++17".to_string()));
        assert!(config.compiler_flags.cxx.contains(&"-fno-rtti".to_string()));
    }

    #[test]
    fn test_linker_flags() {
        let config = get_teensy_config().unwrap();
        assert!(config.linker_flags.contains(&"-mcpu=cortex-m7".to_string()));
        assert!(config
            .linker_flags
            .contains(&"-Wl,--gc-sections".to_string()));
    }

    #[test]
    fn test_linker_libs() {
        let config = get_teensy_config().unwrap();
        assert!(config.linker_libs.contains(&"-lgcc".to_string()));
        assert!(config.linker_libs.contains(&"-lstdc++".to_string()));
        assert!(config.linker_libs.contains(&"-lm".to_string()));
        assert!(config.linker_libs.contains(&"-lc".to_string()));
    }

    #[test]
    fn test_objcopy_config() {
        let config = get_teensy_config().unwrap();
        assert_eq!(config.objcopy.output_format, "ihex");
        assert!(config
            .objcopy
            .remove_sections
            .contains(&".eeprom".to_string()));
    }

    #[test]
    fn test_profiles() {
        let config = get_teensy_config().unwrap();
        let release = config.get_profile("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));
        assert!(release.compile_flags.contains(&"-flto=auto".to_string()));

        let quick = config.get_profile("quick").unwrap();
        assert!(quick.compile_flags.contains(&"-Os".to_string()));
        assert!(quick.link_flags.is_empty());
    }

    #[test]
    fn test_teensy_loader_config() {
        let config = get_teensy_config().unwrap();
        assert_eq!(config.teensy_loader.wait_flag, "-w");
        assert_eq!(config.teensy_loader.verbose_flag, "-v");
        assert_eq!(config.teensy_loader.timeout_secs, 60);
    }
}
