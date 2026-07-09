//! STM32 include-path and flag-handling helpers.
//!
//! Extracted from `orchestrator.rs` (see [`super`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Add STM32duino system include directories for CMSIS and HAL.
///
/// The STM32duino core bundles CMSIS and HAL drivers under `system/`:
/// - `system/Drivers/CMSIS/Device/ST/<family>/Include/` — MCU device headers
/// - `system/Drivers/CMSIS/Core/Include/` — ARM CMSIS core headers
/// - `system/Drivers/<family>_HAL_Driver/Inc/` — STM32 HAL headers
/// - `system/<family>/` — System startup headers
pub(super) fn add_stm32_system_includes(
    system_dir: &Path,
    family: &str,
    include_dirs: &mut Vec<std::path::PathBuf>,
) {
    let drivers = system_dir.join("Drivers");

    // CMSIS Device headers (stm32f1xx.h, stm32f103xb.h, etc.)
    let cmsis_device = drivers
        .join("CMSIS")
        .join("Device")
        .join("ST")
        .join(family)
        .join("Include");
    if cmsis_device.exists() {
        include_dirs.push(cmsis_device);
    }

    // CMSIS Core headers (core_cm3.h, core_cm4.h, etc.)
    let cmsis_core = drivers.join("CMSIS").join("Core").join("Include");
    if cmsis_core.exists() {
        include_dirs.push(cmsis_core);
    }

    // CMSIS startup templates (startup_stm32f103xb.s, etc.)
    let cmsis_startup = drivers
        .join("CMSIS")
        .join("Device")
        .join("ST")
        .join(family)
        .join("Source")
        .join("Templates")
        .join("gcc");
    if cmsis_startup.exists() {
        include_dirs.push(cmsis_startup);
    }

    // HAL Driver headers (stm32f1xx_hal.h, etc.)
    let hal_driver = drivers.join(format!("{family}_HAL_Driver"));
    let hal_inc = hal_driver.join("Inc");
    if hal_inc.exists() {
        include_dirs.push(hal_inc);
    }
    // HAL Driver sources — SrcWrapper's stm32yyxx_hal.c does #include "stm32f1xx_hal.c"
    let hal_src = hal_driver.join("Src");
    if hal_src.exists() {
        include_dirs.push(hal_src);
    }

    // System family directory (startup and system config headers)
    let system_family = system_dir.join(family);
    if system_family.exists() {
        include_dirs.push(system_family);
    }
}

pub(super) fn dedupe_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
}

pub(super) fn dedupe_strings(flags: &mut Vec<String>) {
    let mut seen = HashSet::new();
    flags.retain(|flag| seen.insert(flag.clone()));
}

pub(super) fn apply_define_flags(flags: &[String], defines: &mut HashMap<String, String>) {
    for flag in flags {
        if let Some(def) = flag.strip_prefix("-D") {
            if let Some((key, val)) = def.split_once('=') {
                defines.insert(key.to_string(), val.to_string());
            } else {
                defines.insert(def.to_string(), "1".to_string());
            }
        }
    }
}

/// Derive the STM32duino generic board define from the MCU name.
///
/// `stm32f103c8t6` → `GENERIC_F103C8TX`
/// `stm32f411ceu6` → `GENERIC_F411CEUX`
///
/// Pattern: strip `stm32` prefix, uppercase, replace last char with `X`.
pub(super) fn stm32_generic_board_define(mcu: &str) -> String {
    let suffix = mcu
        .to_lowercase()
        .strip_prefix("stm32")
        .unwrap_or(&mcu.to_lowercase())
        .to_uppercase();
    // Replace last character (pin-count digit) with X
    let mut chars: Vec<char> = suffix.chars().collect();
    if let Some(last) = chars.last_mut() {
        *last = 'X';
    }
    let trimmed: String = chars.into_iter().collect();
    format!("GENERIC_{trimmed}")
}
