//! Data-driven nRF51 MCU configuration from embedded JSON.
//!
//! Every nRF51 part (nrf51822, nrf51422, ...) is a Cortex-M0 without an FPU,
//! so one shared [`ArmMcuConfig`] covers the series. Board-specific details
//! (linker script, memory limits, clock source) come from `BoardConfig`.

use fbuild_core::Result;

use crate::generic_arm::ArmMcuConfig;

const NRF51_JSON: &str = include_str!("configs/nrf51.json");

/// Load the nRF51 MCU configuration for a board MCU (e.g. `nrf51822`).
pub fn get_nrf51_config_for_mcu(mcu: &str) -> Result<ArmMcuConfig> {
    if !mcu.to_ascii_lowercase().starts_with("nrf51") {
        return Err(fbuild_core::FbuildError::ConfigError(format!(
            "unsupported nRF51 MCU: '{}' (expected an nrf51xxx part)",
            mcu
        )));
    }
    serde_json::from_str(NRF51_JSON).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse nRF51 MCU config for '{}': {}",
            mcu, e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nrf51822_is_cortex_m0_without_fpu() {
        let config = get_nrf51_config_for_mcu("nrf51822").unwrap();
        assert_eq!(config.architecture, "arm-cortex-m0");
        let common = &config.compiler_flags.common;
        assert!(common.contains(&"-mcpu=cortex-m0".to_string()));
        assert!(common.contains(&"-mthumb".to_string()));
        assert!(!common.iter().any(|f| f.starts_with("-mfpu")));
        assert!(!common.iter().any(|f| f.starts_with("-mfloat-abi=hard")));
        assert!(config.linker_flags.contains(&"-mcpu=cortex-m0".to_string()));
        assert_eq!(config.objcopy.output_format, "ihex");
    }

    #[test]
    fn defines_match_platformio_arduino_nrf5_builder() {
        let defines = get_nrf51_config_for_mcu("NRF51822").unwrap().defines_map();
        assert_eq!(defines.get("ARDUINO"), Some(&"10805".to_string()));
        assert!(defines.contains_key("ARDUINO_ARCH_NRF5"));
        assert!(defines.contains_key("NRF5"));
        assert!(defines.contains_key("NRF51"));
    }

    #[test]
    fn rejects_non_nrf51_mcu() {
        assert!(get_nrf51_config_for_mcu("nrf52840").is_err());
    }
}
