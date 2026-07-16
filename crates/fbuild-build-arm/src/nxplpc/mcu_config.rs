//! Data-driven NXP LPC8xx MCU configuration from embedded JSON.
//!
//! Both LPC804 and LPC845 are Cortex-M0+ targets and share the same compiler /
//! linker flag set, so the heavy lifting comes from a single JSON blob
//! (`nxplpc.json`). Per-chip differences — primarily preprocessor defines that
//! drivers use to select chip-specific code paths — are layered on top of the
//! shared base by `get_lpc804_config` / `get_lpc845_config`.
//!
//! Tracking: FastLED/FastLED#2845 (Stage 3 — per-chip mcu_config split).

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

use crate::compiler::{CompilerFlags, McuConfig, ObjcopyConfig, ProfileFlags};
use crate::generic_arm::ArmMcuConfig;
use crate::mcu_config::DefineEntry;

const NXPLPC_JSON: &str = include_str!("configs/nxplpc.json");

/// Complete NXP LPC8xx MCU configuration parsed from JSON, with per-chip
/// defines folded into `defines` after JSON deserialization.
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

/// Parse the shared base config from embedded JSON.
fn parse_base_config() -> Result<NxpLpcMcuConfig> {
    serde_json::from_str(NXPLPC_JSON).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse NXP LPC8xx MCU config: {}",
            e
        ))
    })
}

/// LPC804-specific configuration.
///
/// Layers LPC804-only defines on top of the shared base:
///   - `__LPC804__=1`        - drivers branch on this to enable PLU paths
///
/// Board-package CPU identifiers come from board metadata.
pub fn get_lpc804_config() -> Result<NxpLpcMcuConfig> {
    let mut config = parse_base_config()?;
    config
        .defines
        .push(DefineEntry::Simple("__LPC804__".to_string()));
    Ok(config)
}

/// LPC845-specific configuration.
///
/// Layers LPC845-only defines on top of the shared base:
///   - `__LPC845__=1`        - drivers branch on this to enable SCT+DMA paths
///
/// Board-package CPU identifiers come from board metadata.
pub fn get_lpc845_config() -> Result<NxpLpcMcuConfig> {
    let mut config = parse_base_config()?;
    config
        .defines
        .push(DefineEntry::Simple("__LPC845__".to_string()));
    Ok(config)
}

/// Dispatch by MCU name.
///
/// Returns LPC804- or LPC845-specific config (each with the shared base flags
/// plus per-chip defines). Anything else is a config error.
pub fn get_nxplpc_config(mcu: &str) -> Result<NxpLpcMcuConfig> {
    match mcu {
        "lpc804" => get_lpc804_config(),
        "lpc845" => get_lpc845_config(),
        other => Err(fbuild_core::FbuildError::ConfigError(format!(
            "unknown NXP LPC8xx MCU '{}'; expected one of: lpc804, lpc845",
            other
        ))),
    }
}

/// Return the same per-MCU configuration shaped as `ArmMcuConfig` so it can
/// flow into the shared `generic_arm::ArmCompiler` / `ArmLinker` pipeline
/// (Stage 2 of #487). `NxpLpcMcuConfig` and `ArmMcuConfig` deserialize from
/// the same JSON shape; this function reparses the embedded JSON directly
/// into `ArmMcuConfig` and folds the per-MCU defines back in.
pub fn get_arm_mcu_config(mcu: &str) -> Result<ArmMcuConfig> {
    let mut config: ArmMcuConfig = serde_json::from_str(NXPLPC_JSON).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse NXP LPC8xx MCU config as ArmMcuConfig: {}",
            e
        ))
    })?;
    match mcu {
        "lpc804" => {
            config
                .defines
                .push(DefineEntry::Simple("__LPC804__".to_string()));
        }
        "lpc845" => {
            config
                .defines
                .push(DefineEntry::Simple("__LPC845__".to_string()));
        }
        other => {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "unknown NXP LPC8xx MCU '{}'; expected one of: lpc804, lpc845",
                other
            )));
        }
    }
    Ok(config)
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
        assert!(
            config
                .compiler_flags
                .common
                .iter()
                .any(|f| f == "-mcpu=cortex-m0plus")
        );
        assert!(config.compiler_flags.common.iter().any(|f| f == "-mthumb"));
        assert!(
            config
                .compiler_flags
                .cxx
                .iter()
                .any(|f| f == "-std=gnu++11")
        );
        assert!(
            !config
                .compiler_flags
                .common
                .iter()
                .any(|f| f == "-mfloat-abi=soft"),
            "nxplpc should mirror ArduinoCore-LPC8xx platform.txt, which does not pass -mfloat-abi"
        );
    }

    #[test]
    fn linker_flags_match_arduino_core_recipe() {
        let config = get_nxplpc_config("lpc845").unwrap();
        assert!(config.linker_flags.iter().any(|f| f == "-Wl,--gc-sections"));
        assert!(
            !config.linker_flags.iter().any(|f| f == "-nostartfiles"),
            "ArduinoCore-LPC8xx platform.txt does not pass -nostartfiles"
        );
        assert!(
            config.linker_libs.iter().any(|f| f == "-lc"),
            "ArduinoCore-LPC8xx links the standard C library name under nano.specs"
        );
        assert!(
            !config.linker_libs.iter().any(|f| f == "-lc_nano"),
            "ArduinoCore-LPC8xx platform.txt uses -lc, not -lc_nano"
        );
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
        assert!(defines.contains_key("ARDUINO_ARCH_LPC8XX"));
    }

    #[test]
    fn lpc804_config_has_lpc804_defines() {
        let config = get_nxplpc_config("lpc804").unwrap();
        let defines = config.defines_map();
        assert!(
            defines.contains_key("__LPC804__"),
            "lpc804 must define __LPC804__ for driver dispatch"
        );
        assert!(
            !defines.contains_key("CPU_LPC804M101JDH24"),
            "board package defines must come from board metadata"
        );
        assert!(
            !defines.contains_key("__LPC845__"),
            "lpc804 must not leak __LPC845__"
        );
    }

    #[test]
    fn lpc845_config_has_lpc845_defines() {
        let config = get_nxplpc_config("lpc845").unwrap();
        let defines = config.defines_map();
        assert!(
            defines.contains_key("__LPC845__"),
            "lpc845 must define __LPC845__ for driver dispatch"
        );
        assert!(
            !defines.contains_key("CPU_LPC845M301JBD48"),
            "board package defines must come from board metadata"
        );
        assert!(
            !defines.contains_key("__LPC804__"),
            "lpc845 must not leak __LPC804__"
        );
    }

    #[test]
    fn unknown_mcu_returns_config_error() {
        let err = get_nxplpc_config("lpc999").unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("lpc999"),
            "error message should name the offending mcu, got: {}",
            msg
        );
        assert!(
            msg.contains("lpc804") && msg.contains("lpc845"),
            "error message should list valid options, got: {}",
            msg
        );
    }

    #[test]
    fn both_chips_share_base_compiler_flags() {
        let lpc804 = get_nxplpc_config("lpc804").unwrap();
        let lpc845 = get_nxplpc_config("lpc845").unwrap();
        assert_eq!(lpc804.compiler_flags.common, lpc845.compiler_flags.common);
        assert_eq!(lpc804.linker_flags, lpc845.linker_flags);
    }
}
