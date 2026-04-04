//! Data-driven Teensy MCU configuration from embedded JSON.
//!
//! Teensy 4.x boards share ARM Cortex-M7 compiler/linker flags. Board-specific
//! details (linker script, memory limits) come from `BoardConfig` at runtime.

use std::collections::HashMap;

use fbuild_core::Result;
use serde::Deserialize;

use crate::compiler::{CompilerFlags, McuConfig, ObjcopyConfig, ProfileFlags};

const TEENSY31_JSON: &str = include_str!("configs/teensy31.json");
const TEENSY3X_JSON: &str = include_str!("configs/teensy3x.json");
const TEENSY4X_JSON: &str = include_str!("configs/teensy4x.json");
const TEENSYLC_JSON: &str = include_str!("configs/teensylc.json");

/// Teensy loader CLI deploy tool configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct TeensyLoaderConfig {
    pub wait_flag: String,
    pub verbose_flag: String,
    pub timeout_secs: u64,
}

/// Complete Teensy MCU configuration parsed from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct TeensyMcuConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub architecture: String,
    pub compiler_flags: CompilerFlags,
    pub linker_flags: Vec<String>,
    pub linker_libs: Vec<String>,
    pub objcopy: ObjcopyConfig,
    pub profiles: HashMap<String, ProfileFlags>,
    pub teensy_loader: TeensyLoaderConfig,
}

impl TeensyMcuConfig {
    /// Get profile flags for a given profile name.
    pub fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

impl McuConfig for TeensyMcuConfig {
    fn compiler_flags(&self) -> &CompilerFlags {
        &self.compiler_flags
    }

    fn get_profile(&self, name: &str) -> Option<&ProfileFlags> {
        self.profiles.get(name)
    }
}

/// Load the Teensy 4.x MCU configuration.
pub fn get_teensy_config() -> Result<TeensyMcuConfig> {
    get_teensy_config_for_mcu("imxrt1062")
}

/// Load the Teensy MCU configuration for a specific MCU.
pub fn get_teensy_config_for_mcu(mcu: &str) -> Result<TeensyMcuConfig> {
    let json = match mcu {
        "imxrt1062" => TEENSY4X_JSON,
        "mk20dx128" | "mk20dx256" => TEENSY31_JSON,
        "mk64fx512" | "mk66fx1m0" => TEENSY3X_JSON,
        "mkl26z64" => TEENSYLC_JSON,
        _ => {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "unsupported Teensy MCU: '{}' (supported: imxrt1062, mk20dx128, mk20dx256, mk64fx512, mk66fx1m0, mkl26z64)",
                mcu
            )));
        }
    };
    serde_json::from_str(json).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "failed to parse Teensy MCU config for '{}': {}",
            mcu, e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_teensy_config_parses() {
        let config = get_teensy_config().unwrap();
        assert_eq!(config.name, "Teensy 4.x");
        assert_eq!(config.architecture, "arm-cortex-m7");
    }

    #[test]
    fn test_compiler_flags_content() {
        let config = get_teensy_config().unwrap();
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mcpu=cortex-m7".to_string()));
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
            .contains(&"-mfpu=fpv5-d16".to_string()));
        assert!(config.compiler_flags.c.contains(&"-std=gnu11".to_string()));
        assert!(config
            .compiler_flags
            .cxx
            .contains(&"-std=gnu++17".to_string()));
        assert!(config.compiler_flags.cxx.contains(&"-fno-rtti".to_string()));
    }

    #[test]
    fn test_linker_flags() {
        let config = get_teensy_config().unwrap();
        assert!(config.linker_flags.contains(&"-mcpu=cortex-m7".to_string()));
        assert!(config
            .linker_flags
            .contains(&"-Wl,--gc-sections".to_string()));
    }

    #[test]
    fn test_linker_libs() {
        let config = get_teensy_config().unwrap();
        assert!(config.linker_libs.contains(&"-lgcc".to_string()));
        assert!(config.linker_libs.contains(&"-lstdc++".to_string()));
        assert!(config.linker_libs.contains(&"-lm".to_string()));
        assert!(config.linker_libs.contains(&"-lc".to_string()));
    }

    #[test]
    fn test_objcopy_config() {
        let config = get_teensy_config().unwrap();
        assert_eq!(config.objcopy.output_format, "ihex");
        assert!(config
            .objcopy
            .remove_sections
            .contains(&".eeprom".to_string()));
    }

    #[test]
    fn test_profiles() {
        let config = get_teensy_config().unwrap();
        let release = config.get_profile("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));
        assert!(release.compile_flags.contains(&"-flto=auto".to_string()));

        let quick = config.get_profile("quick").unwrap();
        assert!(quick.compile_flags.contains(&"-Os".to_string()));
        assert!(quick.link_flags.is_empty());
    }

    #[test]
    fn test_teensy_loader_config() {
        let config = get_teensy_config().unwrap();
        assert_eq!(config.teensy_loader.wait_flag, "-w");
        assert_eq!(config.teensy_loader.verbose_flag, "-v");
        assert_eq!(config.teensy_loader.timeout_secs, 60);
    }

    #[test]
    fn test_teensy_config_for_mcu_imxrt1062() {
        let config = get_teensy_config_for_mcu("imxrt1062").expect("teensy4x config");
        assert_eq!(config.architecture, "arm-cortex-m7");
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mcpu=cortex-m7".to_string()));
    }

    #[test]
    fn test_teensy_config_for_mcu_mk20dx256() {
        let config = get_teensy_config_for_mcu("mk20dx256").expect("teensy3x config");
        assert_eq!(config.architecture, "arm-cortex-m4");
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mcpu=cortex-m4".to_string()));
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mthumb".to_string()));
    }

    #[test]
    fn test_teensy_config_for_mcu_mkl26z64() {
        let config = get_teensy_config_for_mcu("mkl26z64").expect("teensylc config");
        assert_eq!(config.architecture, "arm-cortex-m0plus");
        assert!(config
            .compiler_flags
            .common
            .contains(&"-mcpu=cortex-m0plus".to_string()));
        assert!(!config
            .compiler_flags
            .common
            .iter()
            .any(|f| f.contains("fpv5")));
    }

    #[test]
    fn test_teensy_config_for_mcu_unsupported() {
        let result = get_teensy_config_for_mcu("unknown_mcu");
        assert!(result.is_err());
    }

    // ── Critical linker flag validation ──────────────────────────────────
    // These tests ensure MCU configs contain platform-critical linker flags
    // that PlatformIO provides. Missing flags cause hard link failures
    // (e.g. undefined reference to `__rtc_localtime`).

    #[test]
    fn test_teensy3x_linker_has_rtc_localtime_defsym() {
        // Teensy 3.x core startup references __rtc_localtime as a weak symbol.
        // Without --defsym the linker fails with "undefined reference to `__rtc_localtime'"
        let config = get_teensy_config_for_mcu("mk66fx1m0").unwrap();
        assert!(
            config
                .linker_flags
                .iter()
                .any(|f| f.contains("__rtc_localtime")),
            "Teensy 3.x linker_flags must include --defsym=__rtc_localtime=0; \
             without it, linking fails on ResetHandler. Flags: {:?}",
            config.linker_flags
        );
    }

    #[test]
    fn test_teensy3x_linker_flags_match_platformio() {
        // Validate Teensy 3.x linker flags contain the critical subset
        // that PlatformIO's Teensy 3.6 builder provides.
        let config = get_teensy_config_for_mcu("mk66fx1m0").unwrap();
        let required = [
            "-mcpu=cortex-m4",
            "-mthumb",
            "-Wl,--gc-sections",
            "-Wl,--defsym=__rtc_localtime=0",
        ];
        for flag in &required {
            assert!(
                config.linker_flags.contains(&flag.to_string()),
                "Teensy 3.x linker_flags missing '{}'. Have: {:?}",
                flag,
                config.linker_flags
            );
        }
    }

    #[test]
    fn test_teensy4x_linker_flags_match_platformio() {
        let config = get_teensy_config_for_mcu("imxrt1062").unwrap();
        let required = [
            "-mcpu=cortex-m7",
            "-mthumb",
            "-mfloat-abi=hard",
            "-mfpu=fpv5-d16",
            "-Wl,--gc-sections",
        ];
        for flag in &required {
            assert!(
                config.linker_flags.contains(&flag.to_string()),
                "Teensy 4.x linker_flags missing '{}'. Have: {:?}",
                flag,
                config.linker_flags
            );
        }
    }

    #[test]
    fn test_teensylc_linker_flags_match_platformio() {
        let config = get_teensy_config_for_mcu("mkl26z64").unwrap();
        let required = ["-mcpu=cortex-m0plus", "-mthumb", "-Wl,--gc-sections"];
        for flag in &required {
            assert!(
                config.linker_flags.contains(&flag.to_string()),
                "Teensy LC linker_flags missing '{}'. Have: {:?}",
                flag,
                config.linker_flags
            );
        }
    }

    /// Validate fbuild's MCU configs against the PlatformIO reference configs.
    ///
    /// The reference configs in configs/reference/ contain the authoritative
    /// linker flags extracted from PlatformIO. If this test fails, either:
    /// - fbuild's MCU config is missing a flag PlatformIO requires (fix the config)
    /// - PlatformIO changed its defaults (regenerate with ci/extract_pio_build_flags.py)
    #[test]
    fn test_linker_flags_match_platformio_reference() {
        let reference_configs: &[(&str, &str)] = &[
            ("mk66fx1m0", include_str!("configs/reference/teensy36.json")),
            ("imxrt1062", include_str!("configs/reference/teensy41.json")),
            ("mkl26z64", include_str!("configs/reference/teensylc.json")),
        ];

        for (mcu, ref_json) in reference_configs {
            let reference: serde_json::Value =
                serde_json::from_str(ref_json).expect("reference JSON should parse");
            let mcu_config = get_teensy_config_for_mcu(mcu)
                .unwrap_or_else(|_| panic!("MCU config should load for {}", mcu));

            let ref_linker_flags: Vec<String> = reference["linker_flags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();

            for flag in &ref_linker_flags {
                assert!(
                    mcu_config.linker_flags.contains(flag),
                    "MCU {} linker_flags missing PlatformIO reference flag '{}'\n\
                     fbuild has:    {:?}\n\
                     PlatformIO has: {:?}",
                    mcu,
                    flag,
                    mcu_config.linker_flags,
                    ref_linker_flags,
                );
            }

            let ref_linker_libs: Vec<String> = reference["linker_libs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();

            for lib in &ref_linker_libs {
                assert!(
                    mcu_config.linker_libs.contains(lib),
                    "MCU {} linker_libs missing PlatformIO reference lib '{}'\n\
                     fbuild has:    {:?}\n\
                     PlatformIO has: {:?}",
                    mcu,
                    lib,
                    mcu_config.linker_libs,
                    ref_linker_libs,
                );
            }
        }
    }

    /// Validate compiler flags against PlatformIO reference (superset check).
    ///
    /// fbuild's MCU config must contain every compiler flag that PlatformIO uses.
    /// fbuild may add extra flags (e.g. -Wextra) beyond what PIO provides.
    #[test]
    fn test_compiler_flags_match_platformio_reference() {
        let reference_configs: &[(&str, &str)] = &[
            ("mk66fx1m0", include_str!("configs/reference/teensy36.json")),
            ("imxrt1062", include_str!("configs/reference/teensy41.json")),
            ("mkl26z64", include_str!("configs/reference/teensylc.json")),
        ];

        for (mcu, ref_json) in reference_configs {
            let reference: serde_json::Value =
                serde_json::from_str(ref_json).expect("reference JSON should parse");
            let mcu_config = get_teensy_config_for_mcu(mcu)
                .unwrap_or_else(|_| panic!("MCU config should load for {}", mcu));

            let ref_cf = &reference["compiler_flags"];
            for category in &["common", "c", "cxx"] {
                let ref_flags: Vec<String> = ref_cf[*category]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect();

                let mcu_flags = match *category {
                    "common" => &mcu_config.compiler_flags.common,
                    "c" => &mcu_config.compiler_flags.c,
                    "cxx" => &mcu_config.compiler_flags.cxx,
                    _ => unreachable!(),
                };

                for flag in &ref_flags {
                    assert!(
                        mcu_flags.contains(flag),
                        "MCU {} compiler_flags.{} missing PlatformIO reference flag '{}'\n\
                         fbuild has:    {:?}\n\
                         PlatformIO has: {:?}",
                        mcu,
                        category,
                        flag,
                        mcu_flags,
                        ref_flags,
                    );
                }
            }
        }
    }

    /// Validate preprocessor defines against PlatformIO reference.
    ///
    /// Defines come from `BoardConfig::get_defines()` (not from MCU config JSON).
    /// This test constructs a BoardConfig for each reference board and checks that
    /// every PIO define is present with the correct value.
    #[test]
    fn test_defines_match_platformio_reference() {
        let reference_configs: &[(&str, &str)] = &[
            ("teensy36", include_str!("configs/reference/teensy36.json")),
            ("teensy41", include_str!("configs/reference/teensy41.json")),
            ("teensylc", include_str!("configs/reference/teensylc.json")),
        ];

        for (board_id, ref_json) in reference_configs {
            let reference: serde_json::Value =
                serde_json::from_str(ref_json).expect("reference JSON should parse");
            let board_config = fbuild_config::BoardConfig::from_board_id(board_id, &HashMap::new())
                .unwrap_or_else(|_| panic!("BoardConfig should load for {}", board_id));
            let actual_defines = board_config.get_defines();

            let ref_defines = reference["defines"]
                .as_object()
                .expect("reference defines should be an object");

            for (name, value) in ref_defines {
                let expected = value.as_str().unwrap();
                let actual = actual_defines.get(name);
                assert!(
                    actual.map(|v| v.as_str()) == Some(expected),
                    "Board {} missing or wrong define {}={}\n\
                     fbuild has: {:?}\n\
                     PlatformIO expects: {}={}",
                    board_id,
                    name,
                    expected,
                    actual,
                    name,
                    expected,
                );
            }
        }
    }

    #[test]
    fn test_teensy31_no_hard_float_flags() {
        // Teensy 3.0/3.1/3.2 (MK20DX) lack an FPU. Hard-float flags cause runtime hard faults.
        for mcu in &["mk20dx128", "mk20dx256"] {
            let config = get_teensy_config_for_mcu(mcu).unwrap();
            for flag in config
                .compiler_flags
                .common
                .iter()
                .chain(&config.linker_flags)
            {
                assert!(
                    !flag.contains("mfloat-abi=hard") && !flag.contains("mfpu="),
                    "MCU {} must not have FPU flags (found '{}') — MK20DX has no FPU",
                    mcu,
                    flag
                );
            }
        }
    }

    #[test]
    fn test_teensy35_36_have_hard_float_flags() {
        // Teensy 3.5/3.6 (MK64FX/MK66FX) have a single-precision FPU.
        for mcu in &["mk64fx512", "mk66fx1m0"] {
            let config = get_teensy_config_for_mcu(mcu).unwrap();
            assert!(
                config
                    .linker_flags
                    .contains(&"-mfloat-abi=hard".to_string()),
                "MCU {} linker_flags should include -mfloat-abi=hard",
                mcu
            );
            assert!(
                config
                    .linker_flags
                    .contains(&"-mfpu=fpv4-sp-d16".to_string()),
                "MCU {} linker_flags should include -mfpu=fpv4-sp-d16",
                mcu
            );
        }
    }

    #[test]
    fn test_all_teensy_mcus_have_consistent_cpu_in_compiler_and_linker() {
        // The -mcpu flag must match between compiler and linker flags.
        // A mismatch (e.g. compiling for cortex-m4 but linking for cortex-m7)
        // produces subtle ABI bugs or hard link failures.
        let mcus = [
            ("imxrt1062", "cortex-m7"),
            ("mk66fx1m0", "cortex-m4"),
            ("mk20dx256", "cortex-m4"),
            ("mkl26z64", "cortex-m0plus"),
        ];
        for (mcu, expected_cpu) in mcus {
            let config = get_teensy_config_for_mcu(mcu).unwrap();
            let expected_flag = format!("-mcpu={}", expected_cpu);
            assert!(
                config.compiler_flags.common.contains(&expected_flag),
                "MCU {} compiler_flags missing {}",
                mcu,
                expected_flag
            );
            assert!(
                config.linker_flags.contains(&expected_flag),
                "MCU {} linker_flags missing {}",
                mcu,
                expected_flag
            );
        }
    }
}
