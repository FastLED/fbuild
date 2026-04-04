//! Data-driven NRF52 MCU configuration from embedded JSON.
//!
//! Nordic NRF52 boards use ARM Cortex-M4F compiler/linker flags. Board-specific
//! details (linker script, memory limits) come from `BoardConfig` at runtime.

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

use crate::compiler::{CompilerFlags, McuConfig, ObjcopyConfig, ProfileFlags};
use crate::esp32::mcu_config::DefineEntry;

const NRF52840_JSON: &str = include_str!("configs/nrf52840.json");

/// Complete NRF52 MCU configuration parsed from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct Nrf52McuConfig {
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

impl Nrf52McuConfig {
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

impl McuConfig for Nrf52McuConfig {
    fn compiler_flags(&self) -> &CompilerFlags {
        &self.compiler_flags
    }

    fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

/// Load the NRF52 MCU configuration for a specific MCU.
pub fn get_nrf52_config_for_mcu(mcu: &str) -> Result<Nrf52McuConfig> {
    let json = match mcu {
        "nrf52840" => NRF52840_JSON,
        _ => {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "unsupported NRF52 MCU: '{}' (supported: nrf52840)",
                mcu
            )));
        }
    };
    serde_json::from_str(json).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse NRF52 MCU config for '{}': {}",
            mcu, e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nrf52840_config_parses() {
        let config = get_nrf52_config_for_mcu("nrf52840").unwrap();
        assert_eq!(config.name, "NRF52840");
        assert_eq!(config.architecture, "arm-cortex-m4f");
    }

    #[test]
    fn test_compiler_flags_content() {
        let config = get_nrf52_config_for_mcu("nrf52840").unwrap();
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mcpu=cortex-m4".to_string()));
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
            .contains(&"-mfpu=fpv4-sp-d16".to_string()));
        assert!(config.compiler_flags.c.contains(&"-std=gnu11".to_string()));
        assert!(config
            .compiler_flags
            .cxx
            .contains(&"-std=gnu++17".to_string()));
    }

    #[test]
    fn test_linker_flags() {
        let config = get_nrf52_config_for_mcu("nrf52840").unwrap();
        assert!(config.linker_flags.contains(&"-mcpu=cortex-m4".to_string()));
        assert!(config
            .linker_flags
            .contains(&"-Wl,--gc-sections".to_string()));
    }

    #[test]
    fn test_linker_libs() {
        let config = get_nrf52_config_for_mcu("nrf52840").unwrap();
        assert!(config.linker_libs.contains(&"-lgcc".to_string()));
        assert!(config.linker_libs.contains(&"-lstdc++".to_string()));
        assert!(config.linker_libs.contains(&"-lm".to_string()));
        assert!(config.linker_libs.contains(&"-lc".to_string()));
    }

    #[test]
    fn test_objcopy_config() {
        let config = get_nrf52_config_for_mcu("nrf52840").unwrap();
        assert_eq!(config.objcopy.output_format, "ihex");
    }

    #[test]
    fn test_profiles() {
        let config = get_nrf52_config_for_mcu("nrf52840").unwrap();
        let release = config.get_profile("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));
        assert!(release.compile_flags.contains(&"-flto".to_string()));

        let quick = config.get_profile("quick").unwrap();
        assert!(quick.compile_flags.contains(&"-Os".to_string()));
        assert!(quick.link_flags.is_empty());
    }

    #[test]
    fn test_nrf52_config_unsupported_mcu() {
        let result = get_nrf52_config_for_mcu("unknown_mcu");
        assert!(result.is_err());
    }

    #[test]
    fn test_nrf52_defines() {
        let config = get_nrf52_config_for_mcu("nrf52840").unwrap();
        let defines = config.defines_map();
        assert_eq!(defines.get("ARDUINO"), Some(&"10808".to_string()));
        assert_eq!(defines.get("NRF52840_XXAA"), Some(&"1".to_string()));
    }
}
