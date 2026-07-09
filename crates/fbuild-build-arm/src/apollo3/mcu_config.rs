//! Data-driven Apollo3 MCU configuration from embedded JSON.
//!
//! Maps Apollo3 MCU names to the appropriate ARM Cortex-M4F configuration.

use fbuild_core::Result;

use crate::generic_arm::ArmMcuConfig;

const APOLLO3_JSON: &str = include_str!("configs/apollo3.json");

/// Load MCU configuration for the Apollo3.
pub fn get_apollo3_config_for_mcu(mcu: &str) -> Result<ArmMcuConfig> {
    let json = match mcu {
        "apollo3" => APOLLO3_JSON,
        _ => {
            // Default to Apollo3 for any Ambiq MCU variant
            if mcu.starts_with("ama3b") || mcu.contains("apollo") {
                APOLLO3_JSON
            } else {
                return Err(fbuild_core::FbuildError::ConfigError(format!(
                    "unsupported Apollo3 MCU: '{}' (supported: apollo3, ama3b*)",
                    mcu
                )));
            }
        }
    };
    serde_json::from_str(json).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse Apollo3 MCU config for '{}': {}",
            mcu, e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_apollo3_config() {
        let config = get_apollo3_config_for_mcu("apollo3").unwrap();
        assert_eq!(config.name, "Apollo3");
        assert_eq!(config.architecture, "arm-cortex-m4f");
    }

    #[test]
    fn test_load_apollo3_ama3b_variant() {
        let config = get_apollo3_config_for_mcu("ama3b1kk").unwrap();
        assert_eq!(config.name, "Apollo3");
    }

    #[test]
    fn test_unsupported_mcu() {
        assert!(get_apollo3_config_for_mcu("stm32f103").is_err());
    }

    #[test]
    fn test_apollo3_compiler_flags() {
        let config = get_apollo3_config_for_mcu("apollo3").unwrap();
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
    }
}
