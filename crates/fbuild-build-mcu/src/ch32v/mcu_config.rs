//! Data-driven CH32V MCU configuration from embedded JSON.
//!
//! CH32V boards use RISC-V compiler/linker flags. Board-specific details
//! (linker script, memory limits) come from `BoardConfig` at runtime.

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

use crate::compiler::{CompilerFlags, McuConfig, ObjcopyConfig, ProfileFlags};
use crate::mcu_config::DefineEntry;

const CH32V003_JSON: &str = include_str!("configs/ch32v003.json");

/// Complete CH32V MCU configuration parsed from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct Ch32vMcuConfig {
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

impl Ch32vMcuConfig {
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

impl McuConfig for Ch32vMcuConfig {
    fn compiler_flags(&self) -> &CompilerFlags {
        &self.compiler_flags
    }

    fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

/// Load the CH32V MCU configuration for a specific MCU series.
///
/// The series is derived from the board JSON `build.series` field
/// (e.g. "ch32v003", "ch32v203", "ch32v307").
pub fn get_ch32v_config_for_mcu(mcu: &str) -> Result<Ch32vMcuConfig> {
    // All CH32V variants currently share the CH32V003 config as a base,
    // with march/mabi overridden from the board JSON extra_flags.
    let json = match mcu {
        "ch32v003" => CH32V003_JSON,
        _ => {
            // For other CH32V series, use the CH32V003 config as a base.
            // The board JSON's extra_flags and march/mabi fields provide
            // the series-specific differences.
            CH32V003_JSON
        }
    };
    serde_json::from_str(json).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse CH32V MCU config for '{}': {}",
            mcu, e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ch32v003_config_parses() {
        let config = get_ch32v_config_for_mcu("ch32v003").unwrap();
        assert_eq!(config.name, "CH32V003");
        assert_eq!(config.architecture, "riscv32");
    }

    #[test]
    fn test_compiler_flags_contain_riscv() {
        let config = get_ch32v_config_for_mcu("ch32v003").unwrap();
        assert!(config
            .compiler_flags
            .common
            .contains(&"-march=rv32ec_zicsr".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mabi=ilp32e".to_string()));
        assert!(config.compiler_flags.c.contains(&"-std=gnu11".to_string()));
        assert!(config
            .compiler_flags
            .cxx
            .contains(&"-std=gnu++17".to_string()));
    }

    #[test]
    fn test_linker_flags() {
        let config = get_ch32v_config_for_mcu("ch32v003").unwrap();
        assert!(config
            .linker_flags
            .contains(&"-march=rv32ec_zicsr".to_string()));
        assert!(config
            .linker_flags
            .contains(&"-Wl,--gc-sections".to_string()));
    }

    #[test]
    fn test_linker_libs() {
        let config = get_ch32v_config_for_mcu("ch32v003").unwrap();
        assert!(config.linker_libs.contains(&"-lgcc".to_string()));
        assert!(config.linker_libs.contains(&"-lstdc++_nano".to_string()));
    }

    #[test]
    fn test_objcopy_config() {
        let config = get_ch32v_config_for_mcu("ch32v003").unwrap();
        assert_eq!(config.objcopy.output_format, "binary");
    }

    #[test]
    fn test_profiles() {
        let config = get_ch32v_config_for_mcu("ch32v003").unwrap();
        let release = config.get_profile("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));
        assert!(release.compile_flags.contains(&"-flto".to_string()));

        let quick = config.get_profile("quick").unwrap();
        assert!(quick.compile_flags.contains(&"-Os".to_string()));
        assert!(quick.link_flags.is_empty());
    }

    #[test]
    fn test_ch32v_defines() {
        let config = get_ch32v_config_for_mcu("ch32v003").unwrap();
        let defines = config.defines_map();
        assert_eq!(defines.get("ARDUINO"), Some(&"10808".to_string()));
    }

    #[test]
    fn test_fallback_config() {
        // Other CH32V series should fall back to CH32V003 config
        let config = get_ch32v_config_for_mcu("ch32v307").unwrap();
        assert_eq!(config.name, "CH32V003");
    }
}
