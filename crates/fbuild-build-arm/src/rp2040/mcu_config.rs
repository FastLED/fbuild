//! Data-driven RP2040/RP2350 MCU configuration from embedded JSON.
//!
//! Maps RP2040/RP2350 MCU names to the appropriate ARM Cortex-M configuration.
//! RP2040 uses Cortex-M0+, RP2350 uses Cortex-M33.

use fbuild_core::Result;

use crate::generic_arm::ArmMcuConfig;

const RP2040_JSON: &str = include_str!("configs/rp2040.json");
const RP2350_JSON: &str = include_str!("configs/rp2350.json");

/// Load MCU configuration for a specific RP2040/RP2350 MCU.
pub fn get_rp2040_config_for_mcu(mcu: &str) -> Result<ArmMcuConfig> {
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
        assert_eq!(config.architecture, "arm-cortex-m0plus");
    }

    #[test]
    fn test_load_rp2350_config() {
        let config = get_rp2040_config_for_mcu("rp2350").unwrap();
        assert_eq!(config.name, "RP2350");
        assert_eq!(config.architecture, "arm-cortex-m33");
    }

    #[test]
    fn test_unsupported_mcu() {
        assert!(get_rp2040_config_for_mcu("rp9999").is_err());
    }

    #[test]
    fn test_rp2040_compiler_flags() {
        let config = get_rp2040_config_for_mcu("rp2040").unwrap();
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mcpu=cortex-m0plus".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mthumb".to_string()));
    }

    #[test]
    fn test_rp2350_has_fpu_flags() {
        let config = get_rp2040_config_for_mcu("rp2350").unwrap();
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mfloat-abi=softfp".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mfpu=fpv5-sp-d16".to_string()));
    }
}
