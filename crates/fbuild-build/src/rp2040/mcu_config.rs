//! Data-driven RP2040/RP2350 MCU configuration from embedded JSON.

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

use crate::compiler::{CompilerFlags, McuConfig, ObjcopyConfig, ProfileFlags};
use crate::esp32::mcu_config::DefineEntry;

const RP2040_JSON: &str = include_str!("configs/rp2040.json");
const RP2350_JSON: &str = include_str!("configs/rp2350.json");

/// RP2040/RP2350 MCU configuration parsed from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct Rp2040McuConfig {
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

impl Rp2040McuConfig {
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

impl McuConfig for Rp2040McuConfig {
    fn compiler_flags(&self) -> &CompilerFlags {
        &self.compiler_flags
    }

    fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

/// Load MCU configuration for a specific RP2040/RP2350 MCU.
pub fn get_rp2040_config_for_mcu(mcu: &str) -> Result<Rp2040McuConfig> {
    let json = match mcu {
        "rp2040" => RP2040_JSON,
        "rp2350" => RP2350_JSON,
        _ => {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "unsupported RP2040 MCU: '{}' (supported: rp2040, rp2350)",
                mcu
            )));
        }
    };
    serde_json::from_str(json).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse RP2040 MCU config for '{}': {}",
            mcu, e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_rp2040_config() {
        let config = get_rp2040_config_for_mcu("rp2040").unwrap();
        assert_eq!(config.name, "RP2040");
    }

    #[test]
    fn test_load_rp2350_config() {
        let config = get_rp2040_config_for_mcu("rp2350").unwrap();
        assert_eq!(config.name, "RP2350");
    }

    #[test]
    fn test_unsupported_mcu() {
        assert!(get_rp2040_config_for_mcu("rp9999").is_err());
    }
}
