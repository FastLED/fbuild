//! Pure fixups that adapt an [`Esp32McuConfig`] to the packages a build
//! actually resolved.
//!
//! The embedded JSON recipes target the newest toolchain and SDK. Older
//! platforms pair them with older compilers and SDKs, so a few flags must go
//! or change. Each rule is a side-effect-free `config -> config` function; the
//! orchestrator applies a composed fixup once, as one line:
//!
//! ```ignore
//! let mcu_config = fixups::for_toolchain(mcu_config, &toolchain_info);
//! let mcu_config = fixups::for_sdk_ld_flags(mcu_config, &sdk_ld_flags);
//! ```

use fbuild_packages::PackageInfo;

use super::mcu_config::Esp32McuConfig;
use crate::compiler::ProfileFlags;

type Rule = fn(Esp32McuConfig) -> Esp32McuConfig;

/// Adapt the recipe to the selected toolchain package.
///
/// - GCC < 14, and ESP32-C2/H2 with GCC 14: the paired SDKs do not define
///   `__dso_handle` for `-fuse-cxa-atexit`-generated references.
/// - Per-MCU Xtensa packages (GCC 8 and 12) reject the atomics switch.
/// - Per-MCU Xtensa GCC 8 (official `espressif32` 6.x/7.x) needs its own
///   linker recipe, older language standards, and no LTO.
pub fn for_toolchain(config: Esp32McuConfig, toolchain: &PackageInfo) -> Esp32McuConfig {
    let legacy_xtensa32 = toolchain.name == "toolchain-xtensa32";
    let per_mcu_xtensa = is_per_mcu_xtensa(&toolchain.name) || legacy_xtensa32;
    let lacks_dso_handle = matches!(config.mcu.as_str(), "esp32c2" | "esp32h2");
    let config = apply_if(
        is_before_gcc14(&toolchain.version) || lacks_dso_handle,
        config,
        drop_cxa_atexit,
    );
    let config = apply_if(per_mcu_xtensa, config, drop_hardware_atomics);
    let config = apply_if(
        (per_mcu_xtensa && is_gcc8(&toolchain.version)) || legacy_xtensa32,
        config,
        legacy_gcc8,
    );
    apply_if(legacy_xtensa32, config, legacy_gcc5)
}

/// GCC 5 in the official legacy ESP32 package predates `-fmacro-prefix-map`.
pub fn supports_macro_prefix_map(toolchain: &PackageInfo) -> bool {
    toolchain.name != "toolchain-xtensa32"
}

/// Drop LTO when the SDK links with `-fno-lto`: objects compiled with LTO
/// would not link.
pub fn for_sdk_ld_flags(config: Esp32McuConfig, sdk_ld_flags: &[String]) -> Esp32McuConfig {
    apply_if(
        sdk_ld_flags.iter().any(|flag| flag == "-fno-lto"),
        config,
        without_lto,
    )
}

fn apply_if(condition: bool, config: Esp32McuConfig, rule: Rule) -> Esp32McuConfig {
    if condition { rule(config) } else { config }
}

fn gcc_major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

fn is_before_gcc14(version: &str) -> bool {
    gcc_major(version).is_some_and(|major| major < 14)
}

fn is_gcc8(version: &str) -> bool {
    version.starts_with("8.")
}

fn is_per_mcu_xtensa(package_name: &str) -> bool {
    package_name.starts_with("toolchain-xtensa-esp32")
}

fn drop_cxa_atexit(mut config: Esp32McuConfig) -> Esp32McuConfig {
    config.compiler_flags.cxx = without(config.compiler_flags.cxx, "-fuse-cxa-atexit");
    config
}

fn drop_hardware_atomics(mut config: Esp32McuConfig) -> Esp32McuConfig {
    config.compiler_flags.common =
        without(config.compiler_flags.common, "-mdisable-hardware-atomics");
    config
}

fn legacy_gcc8(config: Esp32McuConfig) -> Esp32McuConfig {
    let mut config = gcc8_linker_recipe(config);
    config.linker_flags = without(config.linker_flags, "-Wl,--no-warn-rwx-segments");
    config.linker_flags = without(config.linker_flags, "-Wl,--wrap=log_printf");
    without_lto(gcc8_language_standards(config))
}

fn legacy_gcc5(mut config: Esp32McuConfig) -> Esp32McuConfig {
    config.compiler_flags.cxx = replaced(config.compiler_flags.cxx, "-std=gnu++11", "-std=gnu++1z");
    config.linker_flags = [
        "-nostdlib",
        "-Wl,-static",
        "-u",
        "call_user_start_cpu0",
        "-Wl,--undefined=uxTopUsedPriority",
        "-Wl,--gc-sections",
        "-Wl,-EL",
        "-u",
        "ld_include_panic_highint_hdl",
        "-u",
        "__cxa_guard_dummy",
        "-u",
        "__cxx_fatal_exception",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    config.linker_scripts = [
        "esp32_out.ld",
        "esp32.common.ld",
        "esp32.rom.ld",
        "esp32.peripherals.ld",
        "esp32.rom.libgcc.ld",
        "esp32.rom.spiram_incompatible_fns.ld",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    config
}

/// Swap in the MCU's `legacy_gcc8` linker flags and scripts, if it has them.
fn gcc8_linker_recipe(mut config: Esp32McuConfig) -> Esp32McuConfig {
    if let Some(recipe) = &config.legacy_gcc8 {
        config.linker_flags = recipe.linker_flags.clone();
        config.linker_scripts = recipe.linker_scripts.clone();
    }
    config
}

/// Downgrade the recipe's `gnu17`/`gnu++2b` to the legacy GCC 8 `gnu99`/`gnu++11`.
fn gcc8_language_standards(mut config: Esp32McuConfig) -> Esp32McuConfig {
    config.compiler_flags.c = replaced(config.compiler_flags.c, "-std=gnu17", "-std=gnu99");
    config.compiler_flags.cxx = replaced(config.compiler_flags.cxx, "-std=gnu++2b", "-std=gnu++11");
    config
}

/// Remove LTO-related flags from every profile.
fn without_lto(mut config: Esp32McuConfig) -> Esp32McuConfig {
    config.profiles = config
        .profiles
        .into_iter()
        .map(|(name, profile)| {
            let profile = ProfileFlags {
                compile_flags: without_lto_flags(profile.compile_flags),
                link_flags: without_lto_flags(profile.link_flags),
            };
            (name, profile)
        })
        .collect();
    config
}

fn without_lto_flags(flags: Vec<String>) -> Vec<String> {
    flags
        .into_iter()
        .filter(|flag| !flag.contains("lto") && flag != "-fuse-linker-plugin")
        .collect()
}

fn without(flags: Vec<String>, unwanted: &str) -> Vec<String> {
    flags.into_iter().filter(|flag| flag != unwanted).collect()
}

fn replaced(flags: Vec<String>, from: &str, to: &str) -> Vec<String> {
    flags
        .into_iter()
        .map(|flag| if flag == from { to.to_string() } else { flag })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esp32::mcu_config::get_mcu_config;

    fn toolchain(name: &str, version: &str) -> PackageInfo {
        PackageInfo {
            name: name.into(),
            version: version.into(),
            url: String::new(),
            install_path: Default::default(),
            checksum: None,
            installed_bytes: None,
        }
    }

    fn has(flags: &[String], flag: &str) -> bool {
        flags.iter().any(|f| f == flag)
    }

    fn has_lto(config: &Esp32McuConfig) -> bool {
        config.profiles.values().any(|p| {
            p.compile_flags
                .iter()
                .chain(&p.link_flags)
                .any(|f| f.contains("lto") || f == "-fuse-linker-plugin")
        })
    }

    #[test]
    fn gcc_major_parses_leading_component() {
        assert_eq!(gcc_major("13.2.0+20240530"), Some(13));
        assert_eq!(gcc_major("8.4.0+2021r2-patch5"), Some(8));
        assert_eq!(gcc_major("riscv32-esp-elf-14.2.0_20241119"), None);
        assert!(is_before_gcc14("12.2.0+20230208"));
        assert!(!is_before_gcc14("14.2.0"));
        assert!(!is_before_gcc14("riscv32-esp-elf-14.2.0_20241119"));
    }

    #[test]
    fn per_mcu_xtensa_toolchain_uses_gcc12_compatible_flags() {
        let base = get_mcu_config("esp32s3").unwrap();
        let legacy = for_toolchain(
            base.clone(),
            &toolchain("toolchain-xtensa-esp32s3", "12.2.0+20230208"),
        );
        let unified = for_toolchain(base, &toolchain("toolchain-xtensa-esp-elf", "14.2.0"));

        assert!(!has(
            &legacy.compiler_flags.common,
            "-mdisable-hardware-atomics"
        ));
        assert!(!has(&legacy.compiler_flags.cxx, "-fuse-cxa-atexit"));
        assert!(has(
            &unified.compiler_flags.common,
            "-mdisable-hardware-atomics"
        ));
        assert!(has(&unified.compiler_flags.cxx, "-fuse-cxa-atexit"));
    }

    #[test]
    fn pre_gcc14_unified_toolchain_drops_cxa_atexit() {
        // pioarduino 53.x: unified registry toolchains at GCC 13.2 on both
        // architectures, with an SDK that lacks `__dso_handle`.
        for (mcu, package) in [
            ("esp32", "toolchain-xtensa-esp-elf"),
            ("esp32c3", "toolchain-riscv32-esp"),
        ] {
            let config = for_toolchain(
                get_mcu_config(mcu).unwrap(),
                &toolchain(package, "13.2.0+20240530"),
            );
            assert!(
                !has(&config.compiler_flags.cxx, "-fuse-cxa-atexit"),
                "{mcu} kept -fuse-cxa-atexit on GCC 13"
            );
        }
    }

    #[test]
    fn esp32c2_h2_sdk_drops_cxa_atexit_on_gcc14() {
        for mcu in ["esp32c2", "esp32h2"] {
            let config = for_toolchain(
                get_mcu_config(mcu).unwrap(),
                &toolchain("toolchain-riscv32-esp", "14.2.0+20241119"),
            );
            assert!(
                !has(&config.compiler_flags.cxx, "-fuse-cxa-atexit"),
                "{mcu} kept -fuse-cxa-atexit without __dso_handle"
            );
        }
    }

    #[test]
    fn unparseable_version_keeps_recipe() {
        // pioarduino 55.x URL toolchain: version is not a bare GCC version.
        let base = get_mcu_config("esp32c6").unwrap();
        let config = for_toolchain(
            base.clone(),
            &toolchain("esp32-riscv-gcc", "riscv32-esp-elf-14.2.0_20241119"),
        );
        assert_eq!(config.compiler_flags.cxx, base.compiler_flags.cxx);
        assert_eq!(config.compiler_flags.common, base.compiler_flags.common);
    }

    #[test]
    fn platformio_gcc8_uses_supported_cpp_standard() {
        let config = for_toolchain(
            get_mcu_config("esp32s3").unwrap(),
            &toolchain("toolchain-xtensa-esp32s3", "8.4.0+2021r2-patch5"),
        );
        assert!(has(&config.compiler_flags.c, "-std=gnu99"));
        assert!(has(&config.compiler_flags.cxx, "-std=gnu++11"));
        assert!(!has(&config.compiler_flags.cxx, "-std=gnu++2b"));
        assert!(has(&config.linker_flags, "-fno-lto"));
        assert!(!has(&config.linker_flags, "-Wl,--no-warn-rwx-segments"));
        assert!(!has(&config.linker_flags, "-Wl,--wrap=log_printf"));
        assert!(has(&config.linker_scripts, "esp32s3.rom.newlib-time.ld"));
        assert!(!has_lto(&config));
    }

    #[test]
    fn platformio_esp32_gcc8_omits_unsupported_linker_option() {
        let config = for_toolchain(
            get_mcu_config("esp32").unwrap(),
            &toolchain("toolchain-xtensa-esp32", "8.4.0+2021r2-patch3"),
        );
        assert!(!has(&config.linker_flags, "-Wl,--no-warn-rwx-segments"));
    }

    #[test]
    fn platformio_legacy_xtensa32_uses_supported_flags() {
        let package = toolchain("toolchain-xtensa32", "2.50200.97");
        let config = for_toolchain(get_mcu_config("esp32").unwrap(), &package);
        assert!(!has(
            &config.compiler_flags.common,
            "-mdisable-hardware-atomics"
        ));
        assert!(has(&config.compiler_flags.cxx, "-std=gnu++1z"));
        assert!(!has(&config.compiler_flags.cxx, "-std=gnu++2b"));
        assert!(!has_lto(&config));
        assert!(!supports_macro_prefix_map(&package));
        assert!(has(&config.linker_flags, "call_user_start_cpu0"));
        assert!(!has(&config.linker_flags, "-Wl,--no-warn-rwx-segments"));
    }

    #[test]
    fn gcc8_rules_need_a_per_mcu_xtensa_package() {
        let base = get_mcu_config("esp32s3").unwrap();
        let config = for_toolchain(
            base.clone(),
            &toolchain("toolchain-xtensa-esp-elf", "8.4.0"),
        );
        assert_eq!(config.linker_flags, base.linker_flags);
        assert_eq!(config.compiler_flags.c, base.compiler_flags.c);
        assert_eq!(has_lto(&config), has_lto(&base));
    }

    #[test]
    fn sdk_fno_lto_strips_profile_lto() {
        let base = get_mcu_config("esp32s3").unwrap();
        let kept = for_sdk_ld_flags(base.clone(), &["-Wl,--gc-sections".into()]);
        let stripped = for_sdk_ld_flags(base.clone(), &["-fno-lto".into()]);
        assert_eq!(has_lto(&kept), has_lto(&base));
        assert!(!has_lto(&stripped));
        assert_eq!(stripped.profiles.len(), base.profiles.len());
    }

    #[test]
    fn rules_are_independent() {
        let base = get_mcu_config("esp32s3").unwrap();
        let dropped = drop_hardware_atomics(base.clone());
        assert!(!has(
            &dropped.compiler_flags.common,
            "-mdisable-hardware-atomics"
        ));
        assert_eq!(dropped.compiler_flags.cxx, base.compiler_flags.cxx);

        let standards = gcc8_language_standards(base.clone());
        assert_eq!(standards.linker_flags, base.linker_flags);
        assert_eq!(standards.compiler_flags.common, base.compiler_flags.common);
    }
}
