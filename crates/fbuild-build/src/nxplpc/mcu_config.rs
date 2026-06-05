//! Data-driven NXP LPC8xx MCU configuration from embedded JSON.
//!
//! Both LPC804 and LPC845 are Cortex-M0+ targets, share the same compiler/linker
//! flag set, and differ only in memory map (carried by the board JSON) and
//! linker script (selected in the Stage-2 orchestrator).

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

use crate::compiler::{CompilerFlags, McuConfig, ObjcopyConfig, ProfileFlags};
use crate::esp32::mcu_config::DefineEntry;

const NXPLPC_JSON: &str = include_str!("configs/nxplpc.json");

/// Complete NXP LPC8xx MCU configuration parsed from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct NxpLpcMcuConfig {
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

impl NxpLpcMcuConfig {
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

impl McuConfig for NxpLpcMcuConfig {
    fn compiler_flags(&self) -> &CompilerFlags {
        &self.compiler_flags
    }

    fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

/// Load the shared NXP LPC8xx MCU configuration.
///
/// Stage 1 ships a single config for both LPC804 and LPC845. If Stage 2
/// needs per-chip variation (e.g. PLU defines on LPC804), branch on `mcu`.
pub fn get_nxplpc_config(_mcu: &str) -> Result<NxpLpcMcuConfig> {
    serde_json::from_str(NXPLPC_JSON).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse NXP LPC8xx MCU config: {}",
            e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nxplpc_config_parses() {
        let config = get_nxplpc_config("lpc804").unwrap();
        assert_eq!(config.architecture, "arm-cortex-m0plus");
    }

    #[test]
    fn compiler_flags_target_cortex_m0plus() {
        let config = get_nxplpc_config("lpc845").unwrap();
        assert!(config
            .compiler_flags
            .common
            .iter()
            .any(|f| f == "-mcpu=cortex-m0plus"));
        assert!(config.compiler_flags.common.iter().any(|f| f == "-mthumb"));
        assert!(config
            .compiler_flags
            .common
            .iter()
            .any(|f| f == "-mfloat-abi=soft"));
    }

    #[test]
    fn linker_flags_include_gc_sections() {
        let config = get_nxplpc_config("lpc845").unwrap();
        assert!(config.linker_flags.iter().any(|f| f == "-Wl,--gc-sections"));
        assert!(config.linker_flags.iter().any(|f| f == "-nostartfiles"));
    }

    #[test]
    fn profiles_define_release_and_quick() {
        let config = get_nxplpc_config("lpc845").unwrap();
        assert!(config.get_profile("release").is_some());
        assert!(config.get_profile("quick").is_some());
    }

    #[test]
    fn defines_map_contains_nxplpc_token() {
        let config = get_nxplpc_config("lpc804").unwrap();
        let defines = config.defines_map();
        assert!(defines.contains_key("__NXPLPC__"));
    }
}
