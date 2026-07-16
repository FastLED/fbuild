//! Data-driven generic ARM MCU configuration from embedded JSON.
//!
//! Used by STM32, RP2040, NRF52, SAM, and other ARM Cortex-M platforms.
//! Each platform loads its own JSON configs and maps MCU names to the
//! appropriate config variant.

use std::collections::HashMap;

use serde::Deserialize;

use crate::compiler::{CompilerFlags, McuConfig, ObjcopyConfig, ProfileFlags};
use crate::mcu_config::DefineEntry;

/// Generic ARM MCU configuration parsed from JSON.
///
/// Same shape as `TeensyMcuConfig` but without the `teensy_loader` field.
/// Suitable for any ARM Cortex-M platform that uses arm-none-eabi-gcc.
#[derive(Debug, Clone, Deserialize)]
pub struct ArmMcuConfig {
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

impl ArmMcuConfig {
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

impl McuConfig for ArmMcuConfig {
    fn compiler_flags(&self) -> &CompilerFlags {
        &self.compiler_flags
    }

    fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json() -> &'static str {
        r#"{
            "name": "TestARM",
            "description": "Test ARM config",
            "architecture": "arm-cortex-m3",
            "compiler_flags": {
                "common": ["-mcpu=cortex-m3", "-mthumb"],
                "c": ["-std=gnu11"],
                "cxx": ["-std=gnu++17", "-fno-exceptions"]
            },
            "linker_flags": ["-mcpu=cortex-m3", "-mthumb", "-Wl,--gc-sections"],
            "linker_libs": ["-lgcc", "-lm"],
            "objcopy": {"output_format": "ihex", "remove_sections": [".eeprom"]},
            "profiles": {
                "release": {"compile_flags": ["-Os"], "link_flags": ["-flto"]},
                "quick": {"compile_flags": ["-O1"], "link_flags": []}
            },
            "defines": [["ARDUINO", "10808"], "MY_FLAG"]
        }"#
    }

    #[test]
    fn test_arm_mcu_config_parses() {
        let config: ArmMcuConfig = serde_json::from_str(sample_json()).unwrap();
        assert_eq!(config.name, "TestARM");
        assert_eq!(config.architecture, "arm-cortex-m3");
    }

    #[test]
    fn test_arm_mcu_config_compiler_flags() {
        let config: ArmMcuConfig = serde_json::from_str(sample_json()).unwrap();
        assert!(
            config
                .compiler_flags
                .common
                .contains(&"-mcpu=cortex-m3".to_string())
        );
        assert!(config.compiler_flags.c.contains(&"-std=gnu11".to_string()));
        assert!(
            config
                .compiler_flags
                .cxx
                .contains(&"-fno-exceptions".to_string())
        );
    }

    #[test]
    fn test_arm_mcu_config_profiles() {
        let config: ArmMcuConfig = serde_json::from_str(sample_json()).unwrap();
        let release = config.get_profile("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));
        assert!(release.link_flags.contains(&"-flto".to_string()));

        let quick = config.get_profile("quick").unwrap();
        assert!(quick.compile_flags.contains(&"-O1".to_string()));
        assert!(quick.link_flags.is_empty());
    }

    #[test]
    fn test_arm_mcu_config_defines_map() {
        let config: ArmMcuConfig = serde_json::from_str(sample_json()).unwrap();
        let defines = config.defines_map();
        assert_eq!(defines.get("ARDUINO"), Some(&"10808".to_string()));
        assert_eq!(defines.get("MY_FLAG"), Some(&"1".to_string()));
    }

    #[test]
    fn test_arm_mcu_config_mcu_config_trait() {
        let config: ArmMcuConfig = serde_json::from_str(sample_json()).unwrap();
        let flags = McuConfig::compiler_flags(&config);
        assert!(flags.common.contains(&"-mthumb".to_string()));
        let profile = McuConfig::get_profile(&config, "release").unwrap();
        assert!(profile.compile_flags.contains(&"-Os".to_string()));
    }
}
