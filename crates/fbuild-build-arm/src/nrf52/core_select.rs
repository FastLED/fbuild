//! nRF52 Arduino core selection (FastLED/fbuild#2017).
//!
//! Every nRF52 board builds against an Adafruit-family core, but not all of
//! them against Adafruit's own: the Seeed XIAO nRF52840 boards need Seeed's
//! fork, the only core that ships their variants. PlatformIO's
//! maxgerhardt/platform-nordicnrf52 picks that package by board name in
//! `platform.py`; fbuild reads it from the board JSON's
//! `build.framework_package` instead, so the choice stays board data.
//!
//! The two cores also link different SoftDevices (Adafruit's boards S140
//! 6.1.1, the XIAO boards S140 7.3.0), so the SoftDevice API headers follow
//! the linker script the board selects, as in the cores' own `platform.txt`
//! where `build.sd_version` and `build.ldscript` are set together.
//!
//! Seeed's core also drops the chip guards Adafruit's wraps around the
//! Nordic MDK system files (`nrfx/mdk/system_nrf52.c` is `NRF52832_XXAA`-only
//! there, `system_nrf52840.c` `NRF52840_XXAA`-only), so both define
//! `SystemInit`. Arduino and PlatformIO archive the core, so the linker pulls
//! only one; fbuild links core objects directly, so it keeps only the
//! board MCU's system file instead.

use std::path::{Path, PathBuf};

use fbuild_packages::library::Nrf52Framework;

/// SoftDevice API directory used when the linker script names no SoftDevice
/// version that the core ships headers for: Adafruit's default S140 6.1.1.
const DEFAULT_SOFTDEVICE_API: &str = "s140_nrf52_6.1.1_API";

/// The core `board` builds against: its `build.framework_package` (which
/// `board_build.framework_package` can override), else Adafruit's.
pub(crate) fn selected_framework(
    board: &fbuild_config::BoardConfig,
) -> fbuild_core::Result<Nrf52Framework> {
    let Some(package) = board.framework_package.as_deref().map(str::trim) else {
        return Ok(Nrf52Framework::Adafruit);
    };
    Nrf52Framework::from_package_name(package).ok_or_else(|| {
        let supported: Vec<&str> = Nrf52Framework::ALL
            .iter()
            .map(|framework| framework.package_name())
            .collect();
        fbuild_core::FbuildError::ConfigError(format!(
            "board `{}` selects nRF52 framework package `{package}`, which fbuild \
             does not support (supported: {})",
            board.board_id,
            supported.join(", ")
        ))
    })
}

/// SoftDevice API directory (`<nordic_dir>/softdevice/<sd>_nrf52_<ver>_API`)
/// matching `linker_script`, e.g. `nrf52840_s140_v7.ld` selects the newest
/// `s140_nrf52_7.*_API` the core ships. Falls back to
/// [`DEFAULT_SOFTDEVICE_API`] when the script names no SoftDevice or the
/// core has no matching headers.
pub(crate) fn softdevice_api_dir(nordic_dir: &Path, linker_script: &Path) -> PathBuf {
    let softdevice_dir = nordic_dir.join("softdevice");
    linker_script
        .file_name()
        .and_then(|name| softdevice_from_ldscript(&name.to_string_lossy()))
        .and_then(|(sd_name, major)| newest_api_dir(&softdevice_dir, &sd_name, major))
        .unwrap_or_else(|| softdevice_dir.join(DEFAULT_SOFTDEVICE_API))
}

/// `nrf52840_s140_v7.ld` -> `("s140", 7)`.
fn softdevice_from_ldscript(file_name: &str) -> Option<(String, u32)> {
    let stem = file_name.strip_suffix(".ld")?;
    let mut parts = stem.split('_').skip(1);
    let sd_name = parts.next()?;
    let digits = sd_name.strip_prefix('s')?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let major = parts.next()?.strip_prefix('v')?.parse().ok()?;
    Some((sd_name.to_string(), major))
}

fn newest_api_dir(softdevice_dir: &Path, sd_name: &str, major: u32) -> Option<PathBuf> {
    let prefix = format!("{sd_name}_nrf52_");
    std::fs::read_dir(softdevice_dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name.strip_prefix(&prefix)?.strip_suffix("_API")?;
            let parsed: Vec<u32> = version
                .split('.')
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            (parsed.first() == Some(&major)).then(|| (parsed, entry.path()))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, path)| path)
}

/// Drop the core's Nordic MDK system files (`nrfx/mdk/system_nrf52*.c`) other
/// than `system_<mcu>.c`, the one Adafruit's chip guards would leave
/// non-empty. A no-op when the core ships no system file for `mcu`.
pub(crate) fn retain_mcu_system_source(core_sources: &mut Vec<PathBuf>, mcu: &str) {
    let wanted = format!("system_{}.c", mcu.to_ascii_lowercase());
    let mdk_dir = Path::new("nrfx").join("mdk");
    let mdk_system_file = |path: &Path| -> Option<String> {
        let name = path.file_name()?.to_string_lossy().into_owned();
        let in_mdk = path.parent().is_some_and(|dir| dir.ends_with(&mdk_dir));
        (in_mdk && name.starts_with("system_nrf52") && name.ends_with(".c")).then_some(name)
    };
    if !core_sources
        .iter()
        .any(|path| mdk_system_file(path).is_some_and(|name| name == wanted))
    {
        return;
    }
    core_sources.retain(|path| mdk_system_file(path).is_none_or(|name| name == wanted));
}

#[cfg(test)]
#[path = "core_select_tests.rs"]
mod tests;
