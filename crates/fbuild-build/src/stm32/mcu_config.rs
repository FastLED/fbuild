//! Data-driven STM32 MCU configuration from embedded JSON.
//!
//! Maps STM32 MCU names to the appropriate ARM Cortex-M configuration.
//! STM32F1xx uses Cortex-M3, STM32F4xx uses Cortex-M4F, STM32H7xx uses Cortex-M7,
//! and STM32U5xx uses Cortex-M33F.

use fbuild_core::Result;

use crate::generic_arm::ArmMcuConfig;

const STM32F1_JSON: &str = include_str!("configs/stm32f1.json");
const STM32F4_JSON: &str = include_str!("configs/stm32f4.json");
const STM32H7_JSON: &str = include_str!("configs/stm32h7.json");
const STM32U5_JSON: &str = include_str!("configs/stm32u5.json");

/// Load the STM32 MCU configuration for a specific MCU.
///
/// Maps MCU name prefixes to the appropriate JSON config:
/// - `stm32f103*` → STM32F1 (Cortex-M3)
/// - `stm32f4*` → STM32F4 (Cortex-M4F)
/// - `stm32h7*` → STM32H7 (Cortex-M7)
/// - `stm32u5*` → STM32U5 (Cortex-M33F)
pub fn get_stm32_config_for_mcu(mcu: &str) -> Result<ArmMcuConfig> {
    let mcu_lower = mcu.to_lowercase();
    let json = if mcu_lower.starts_with("stm32f103") {
        STM32F1_JSON
    } else if mcu_lower.starts_with("stm32f4") {
        STM32F4_JSON
    } else if mcu_lower.starts_with("stm32h7") {
        STM32H7_JSON
    } else if mcu_lower.starts_with("stm32u5") {
        STM32U5_JSON
    } else {
        return Err(fbuild_core::FbuildError::ConfigError(format!(
            "unsupported STM32 MCU: '{}' (supported prefixes: stm32f103, stm32f4, stm32h7, stm32u5/stm32u585)",
            mcu
        )));
    };
    serde_json::from_str(json).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse STM32 MCU config for '{}': {}",
            mcu, e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stm32f1_config_parses() {
        let config = get_stm32_config_for_mcu("stm32f103c8").unwrap();
        assert_eq!(config.name, "STM32F1");
        assert_eq!(config.architecture, "arm-cortex-m3");
    }

    #[test]
    fn test_stm32f4_config_parses() {
        let config = get_stm32_config_for_mcu("stm32f401re").unwrap();
        assert_eq!(config.name, "STM32F4");
        assert_eq!(config.architecture, "arm-cortex-m4f");
    }

    #[test]
    fn test_stm32h7_config_parses() {
        let config = get_stm32_config_for_mcu("stm32h743zi").unwrap();
        assert_eq!(config.name, "STM32H7");
        assert_eq!(config.architecture, "arm-cortex-m7");
    }

    #[test]
    fn test_stm32u5_config_parses() {
        let config = get_stm32_config_for_mcu("stm32u585zit6q").unwrap();
        assert_eq!(config.name, "STM32U5");
        assert_eq!(config.architecture, "arm-cortex-m33");
    }

    #[test]
    fn test_stm32f1_no_fpu_flags() {
        let config = get_stm32_config_for_mcu("stm32f103c8").unwrap();
        for flag in config
            .compiler_flags
            .common
            .iter()
            .chain(&config.linker_flags)
        {
            assert!(
                !flag.contains("mfloat-abi=hard") && !flag.contains("mfpu="),
                "STM32F1 must not have FPU flags (found '{}') -- Cortex-M3 has no FPU",
                flag
            );
        }
    }

    #[test]
    fn test_stm32f4_has_fpu_flags() {
        let config = get_stm32_config_for_mcu("stm32f401re").unwrap();
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mfloat-abi=hard".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mfpu=fpv4-sp-d16".to_string()));
    }

    #[test]
    fn test_stm32h7_has_fpu_flags() {
        let config = get_stm32_config_for_mcu("stm32h743zi").unwrap();
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mfloat-abi=hard".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mfpu=fpv5-d16".to_string()));
    }

    #[test]
    fn test_stm32u5_has_platformio_hard_float_flags() {
        let config = get_stm32_config_for_mcu("stm32u585zit6q").unwrap();
        for flag in [
            "-mcpu=cortex-m33",
            "-mthumb",
            "-mfpu=fpv4-sp-d16",
            "-mfloat-abi=hard",
        ] {
            let flag = flag.to_string();
            assert!(
                config.compiler_flags.common.contains(&flag),
                "STM32U5 compiler flags missing {flag}"
            );
            assert!(
                config.linker_flags.contains(&flag),
                "STM32U5 linker flags missing {flag}"
            );
        }
    }

    #[test]
    fn test_stm32_unsupported_mcu() {
        let result = get_stm32_config_for_mcu("unknown_mcu");
        assert!(result.is_err());
    }

    #[test]
    fn test_stm32f1_compiler_flags() {
        let config = get_stm32_config_for_mcu("stm32f103c8").unwrap();
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mcpu=cortex-m3".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mthumb".to_string()));
        assert!(config.compiler_flags.c.contains(&"-std=gnu11".to_string()));
        assert!(config
            .compiler_flags
            .cxx
            .contains(&"-std=gnu++17".to_string()));
    }

    #[test]
    fn test_stm32f1_linker_flags() {
        let config = get_stm32_config_for_mcu("stm32f103c8").unwrap();
        assert!(config.linker_flags.contains(&"-mcpu=cortex-m3".to_string()));
        assert!(config
            .linker_flags
            .contains(&"-Wl,--gc-sections".to_string()));
    }

    #[test]
    fn test_stm32_profiles() {
        let config = get_stm32_config_for_mcu("stm32f103c8").unwrap();
        let release = config.get_profile("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));
        let quick = config.get_profile("quick").unwrap();
        assert!(quick.compile_flags.contains(&"-Os".to_string()));
        assert!(quick.link_flags.is_empty());
    }

    #[test]
    fn test_stm32_case_insensitive() {
        let config = get_stm32_config_for_mcu("STM32F103C8").unwrap();
        assert_eq!(config.name, "STM32F1");
    }

    #[test]
    fn test_stm32u5_case_insensitive() {
        let config = get_stm32_config_for_mcu("STM32U585ZIT6Q").unwrap();
        assert_eq!(config.name, "STM32U5");
    }
}
