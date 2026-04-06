//! Data-driven SAM MCU configuration from embedded JSON.
//!
//! Atmel SAM boards use ARM Cortex-M3 compiler/linker flags. Board-specific
//! details (linker script, memory limits) come from `BoardConfig` at runtime.

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

use crate::compiler::{CompilerFlags, McuConfig, ObjcopyConfig, ProfileFlags};
use crate::esp32::mcu_config::DefineEntry;

const SAM3X_JSON: &str = include_str!("configs/sam3x.json");
const SAMD21_JSON: &str = include_str!("configs/samd21.json");
const SAMD51_JSON: &str = include_str!("configs/samd51.json");

/// Complete SAM MCU configuration parsed from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct SamMcuConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub architecture: String,
    pub compiler_flags: CompilerFlags,
    pub linker_flags: Vec<String>,
    pub linker_libs: Vec<String>,
    pub objcopy: ObjcopyConfig,
    pub profiles: HashMap<String, ProfileFlags>,
    #[serde(default)]
    pub defines: Vec<DefineEntry>,
}

impl SamMcuConfig {
    /// Get profile flags for a given profile name.
    pub fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }

    /// Convert defines to a HashMap suitable for merging with board defines.
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
}

impl McuConfig for SamMcuConfig {
    fn compiler_flags(&self) -> &CompilerFlags {
        &self.compiler_flags
    }

    fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

/// Load the SAM MCU configuration for a specific MCU.
pub fn get_sam_config_for_mcu(mcu: &str) -> Result<SamMcuConfig> {
    let json = match mcu {
        "at91sam3x8e" => SAM3X_JSON,
        m if m.starts_with("samd21") => SAMD21_JSON,
        m if m.starts_with("samd51") => SAMD51_JSON,
        _ => {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "unsupported SAM MCU: '{}' (supported: at91sam3x8e, samd21*, samd51*)",
                mcu
            )));
        }
    };
    serde_json::from_str(json).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse SAM MCU config for '{}': {}",
            mcu, e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sam3x_config_parses() {
        let config = get_sam_config_for_mcu("at91sam3x8e").unwrap();
        assert_eq!(config.name, "SAM3X");
        assert_eq!(config.architecture, "arm-cortex-m3");
    }

    #[test]
    fn test_compiler_flags_content() {
        let config = get_sam_config_for_mcu("at91sam3x8e").unwrap();
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mcpu=cortex-m3".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mthumb".to_string()));
        assert!(config.compiler_flags.c.contains(&"-std=gnu11".to_string()));
        assert!(config
            .compiler_flags
            .cxx
            .contains(&"-std=gnu++17".to_string()));
    }

    #[test]
    fn test_linker_flags() {
        let config = get_sam_config_for_mcu("at91sam3x8e").unwrap();
        assert!(config.linker_flags.contains(&"-mcpu=cortex-m3".to_string()));
        assert!(config
            .linker_flags
            .contains(&"-Wl,--gc-sections".to_string()));
    }

    #[test]
    fn test_linker_libs() {
        let config = get_sam_config_for_mcu("at91sam3x8e").unwrap();
        assert!(config.linker_libs.contains(&"-lgcc".to_string()));
        assert!(config.linker_libs.contains(&"-lstdc++".to_string()));
        assert!(config.linker_libs.contains(&"-lm".to_string()));
        assert!(config.linker_libs.contains(&"-lc".to_string()));
    }

    #[test]
    fn test_objcopy_config() {
        let config = get_sam_config_for_mcu("at91sam3x8e").unwrap();
        assert_eq!(config.objcopy.output_format, "binary");
    }

    #[test]
    fn test_profiles() {
        let config = get_sam_config_for_mcu("at91sam3x8e").unwrap();
        let release = config.get_profile("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));

        let quick = config.get_profile("quick").unwrap();
        assert!(quick.compile_flags.contains(&"-Os".to_string()));
        assert!(quick.link_flags.is_empty());
    }

    #[test]
    fn test_sam_config_unsupported_mcu() {
        let result = get_sam_config_for_mcu("unknown_mcu");
        assert!(result.is_err());
    }

    #[test]
    fn test_sam_defines() {
        let config = get_sam_config_for_mcu("at91sam3x8e").unwrap();
        let defines = config.defines_map();
        assert_eq!(defines.get("ARDUINO"), Some(&"10808".to_string()));
    }

    #[test]
    fn test_samd21_config_parses() {
        let config = get_sam_config_for_mcu("samd21g18a").unwrap();
        assert_eq!(config.name, "SAMD21");
        assert_eq!(config.architecture, "arm-cortex-m0plus");
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mcpu=cortex-m0plus".to_string()));
    }

    #[test]
    fn test_samd51_config_parses() {
        let config = get_sam_config_for_mcu("samd51j19a").unwrap();
        assert_eq!(config.name, "SAMD51");
        assert_eq!(config.architecture, "arm-cortex-m4f");
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mcpu=cortex-m4".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mfloat-abi=hard".to_string()));
    }

    #[test]
    fn test_samd51p_config_parses() {
        let config = get_sam_config_for_mcu("samd51p20a").unwrap();
        assert_eq!(config.name, "SAMD51");
    }
}
