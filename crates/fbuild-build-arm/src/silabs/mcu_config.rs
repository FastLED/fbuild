//! Data-driven Silicon Labs MCU configuration from embedded JSON.
//!
//! Silicon Labs boards use ARM Cortex-M33 compiler/linker flags. Board-specific
//! details (linker script, memory limits) come from `BoardConfig` at runtime.

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

use crate::compiler::{CompilerFlags, McuConfig, ObjcopyConfig, ProfileFlags};
use crate::mcu_config::DefineEntry;

const EFR32MG24_JSON: &str = include_str!("configs/efr32mg24.json");

/// Complete Silicon Labs MCU configuration parsed from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct SilabsMcuConfig {
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

impl SilabsMcuConfig {
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

impl McuConfig for SilabsMcuConfig {
    fn compiler_flags(&self) -> &CompilerFlags {
        &self.compiler_flags
    }

    fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

/// Load the Silicon Labs MCU configuration for a specific MCU.
pub fn get_silabs_config_for_mcu(mcu: &str) -> Result<SilabsMcuConfig> {
    let json = match mcu {
        m if m.contains("cortex-m33") || m.contains("efr32") || m == "efr32mg24" => EFR32MG24_JSON,
        _ => {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "unsupported Silicon Labs MCU: '{}' (supported: efr32mg24)",
                mcu
            )));
        }
    };
    serde_json::from_str(json).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse Silicon Labs MCU config for '{}': {}",
            mcu, e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_efr32mg24_config_parses() {
        let config = get_silabs_config_for_mcu("efr32mg24").unwrap();
        assert_eq!(config.name, "EFR32MG24");
        assert_eq!(config.architecture, "arm-cortex-m33");
    }

    #[test]
    fn test_compiler_flags_content() {
        let config = get_silabs_config_for_mcu("efr32mg24").unwrap();
        assert!(
            config
                .compiler_flags
                .common
                .contains(&"-mcpu=cortex-m33".to_string())
        );
        assert!(
            config
                .compiler_flags
                .common
                .contains(&"-mthumb".to_string())
        );
        assert!(
            config
                .compiler_flags
                .common
                .contains(&"-mfloat-abi=hard".to_string())
        );
        assert!(
            config
                .compiler_flags
                .common
                .contains(&"-mfpu=fpv5-sp-d16".to_string())
        );
        assert!(config.compiler_flags.c.contains(&"-std=gnu11".to_string()));
        assert!(
            config
                .compiler_flags
                .cxx
                .contains(&"-std=gnu++17".to_string())
        );
    }

    #[test]
    fn test_linker_flags() {
        let config = get_silabs_config_for_mcu("efr32mg24").unwrap();
        assert!(
            config
                .linker_flags
                .contains(&"-mcpu=cortex-m33".to_string())
        );
        assert!(
            config
                .linker_flags
                .contains(&"-Wl,--gc-sections".to_string())
        );
    }

    #[test]
    fn test_linker_libs() {
        let config = get_silabs_config_for_mcu("efr32mg24").unwrap();
        assert!(config.linker_libs.contains(&"-lgcc".to_string()));
        assert!(config.linker_libs.contains(&"-lstdc++".to_string()));
        assert!(config.linker_libs.contains(&"-lm".to_string()));
        assert!(config.linker_libs.contains(&"-lc".to_string()));
    }

    #[test]
    fn test_objcopy_config() {
        let config = get_silabs_config_for_mcu("efr32mg24").unwrap();
        assert_eq!(config.objcopy.output_format, "binary");
    }

    #[test]
    fn test_profiles() {
        let config = get_silabs_config_for_mcu("efr32mg24").unwrap();
        let release = config.get_profile("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));
        assert!(release.compile_flags.contains(&"-flto".to_string()));

        let quick = config.get_profile("quick").unwrap();
        assert!(quick.compile_flags.contains(&"-Os".to_string()));
        assert!(quick.link_flags.is_empty());
    }

    #[test]
    fn test_silabs_config_unsupported_mcu() {
        let result = get_silabs_config_for_mcu("unknown_mcu");
        assert!(result.is_err());
    }

    #[test]
    fn test_silabs_defines() {
        let config = get_silabs_config_for_mcu("efr32mg24").unwrap();
        let defines = config.defines_map();
        assert_eq!(defines.get("ARDUINO"), Some(&"10808".to_string()));
    }

    #[test]
    fn test_silabs_config_cortex_m33_mcu_match() {
        // Should match MCU strings containing "cortex-m33"
        let config = get_silabs_config_for_mcu("cortex-m33").unwrap();
        assert_eq!(config.name, "EFR32MG24");
    }

    #[test]
    fn test_silabs_config_efr32_prefix_match() {
        // Should match MCU strings containing "efr32"
        let config = get_silabs_config_for_mcu("efr32bg22").unwrap();
        assert_eq!(config.name, "EFR32MG24");
    }
}
