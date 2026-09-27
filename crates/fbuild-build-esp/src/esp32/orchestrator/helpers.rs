//! Helper functions for the ESP32 orchestrator: failure markers, fingerprinting,
//! and small utilities used across orchestration phases.
//!
//! Flag merging primitives (`apply_user_flags`, `apply_overlay_flags`) used to
//! live here and were shared across ESP32 orchestration phases. They were
//! lifted to `crate::flag_overlay` so the NXP LPC8xx orchestrator (and any
//! future platform that compiles its libraries against the `[env:*] build_flags`
//! overlay) can reach them without depending on `esp32::orchestrator::helpers`.
//! See FastLED/fbuild#587.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::Result;

/// Record the requested ESP32 package pins beside the installed stack so a
/// successful build cannot conceal a cross-version framework or SDK.
pub(super) fn esp32_package_stack_summary(
    env_config: Option<&HashMap<String, String>>,
    platform: &fbuild_packages::PackageInfo,
    framework: &fbuild_packages::PackageInfo,
    toolchain: &fbuild_packages::PackageInfo,
    installed_framework: &fbuild_packages::library::Esp32Framework,
    mcu: &str,
) -> String {
    let requested_platform = env_config
        .and_then(|env| env.get("platform"))
        .map_or("<default>", String::as_str);
    let requested_framework = env_config
        .and_then(|env| env.get("platform_packages"))
        .and_then(|raw| {
            raw.lines().find(|line| {
                line.trim()
                    .split_once('@')
                    .is_some_and(|(name, _)| name.trim().ends_with("framework-arduinoespressif32"))
            })
        })
        .map_or("<platform manifest>", str::trim);
    let sdk_version = installed_framework
        .bundled_esp_idf_version(mcu)
        .unwrap_or_else(|| "unknown (bundled with framework)".to_string());
    format!(
        "ESP32 packages: requested platform={requested_platform}, framework={requested_framework}; resolved platform={}@{}, framework={}@{}, toolchain={}@{}, ESP-IDF SDK={sdk_version}",
        platform.name,
        platform.version,
        framework.name,
        framework.version,
        toolchain.name,
        toolchain.version,
    )
}

/// Resolve the image settings once so the fingerprint and linker use identical values.
pub(super) fn flash_settings(
    board: &fbuild_config::BoardConfig,
    mcu_config: &crate::esp32::mcu_config::Esp32McuConfig,
) -> (String, String, String) {
    let f_for_image = board.f_image.as_deref().or(board.f_flash.as_deref());
    let flash_freq = crate::esp32::esp32_linker::f_flash_to_esptool_freq(
        f_for_image,
        mcu_config.default_flash_freq(),
    );
    let flash_mode = board
        .flash_mode
        .clone()
        .unwrap_or_else(|| mcu_config.default_flash_mode().to_string());
    let flash_size = crate::esp32::mcu_config::bytes_to_flash_size(
        board.max_flash,
        mcu_config.default_flash_size(),
    )
    .to_string();
    (flash_freq, flash_mode, flash_size)
}

/// Arduino's packaged ESP-IDF libraries cannot reflect sdkconfig changes without
/// a hybrid IDF rebuild. Reject overlays before context setup has side effects.
pub(super) fn reject_unsupported_sdkconfig_overlay(
    config: &fbuild_config::PlatformIOConfig,
    env_name: &str,
) -> Result<()> {
    let env = config.get_env_config(env_name)?;
    if !env
        .get("framework")
        .is_some_and(|framework| framework.split(',').any(|part| part.trim() == "arduino"))
    {
        return Ok(());
    }
    for key in ["board_build.sdkconfig_defaults", "custom_sdkconfig"] {
        if env.get(key).is_some_and(|value| !value.trim().is_empty()) {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "{key} is unsupported for ESP32 Arduino environment '{env_name}': fbuild cannot apply an sdkconfig overlay to precompiled ESP-IDF libraries (see FastLED/fbuild#1460)"
            )));
        }
    }
    Ok(())
}

/// Apply the effective `-D` / `-U` compiler flags used by library selection.
/// SDK flags are inherited before `build_unflags`; user flags apply afterward
/// unless an exact user token is also unflagged by the compiler.
pub(super) fn apply_effective_define_flags(
    defines: &mut HashMap<String, String>,
    sdk_flags: &[String],
    user_flags: &[String],
    build_unflags: &[String],
) {
    apply_define_flags(defines, sdk_flags, build_unflags);
    for flag in build_unflags {
        if let Some(name) = define_name(flag) {
            defines.remove(name);
        }
    }
    apply_define_flags(defines, user_flags, build_unflags);
}

fn apply_define_flags(
    defines: &mut HashMap<String, String>,
    flags: &[String],
    build_unflags: &[String],
) {
    for flag in flags {
        if build_unflags.contains(flag) {
            continue;
        }
        if let Some((name, value)) = define_value(flag) {
            defines.insert(name.to_string(), value.to_string());
        } else if let Some(name) = flag.strip_prefix("-U") {
            defines.remove(name.trim());
        }
    }
}

fn define_name(flag: &str) -> Option<&str> {
    let raw = flag.strip_prefix("-D")?;
    let name = raw.split_once('=').map_or(raw, |(name, _)| name).trim();
    (!name.is_empty()).then_some(name)
}

fn define_value(flag: &str) -> Option<(&str, &str)> {
    let raw = flag.strip_prefix("-D")?.trim();
    let (name, value) = raw.split_once('=').unwrap_or((raw, "1"));
    let name = name.trim();
    (!name.is_empty()).then_some((name, value))
}

pub(super) fn framework_failure_marker(build_dir: &Path, lib_name: &str) -> PathBuf {
    build_dir.join(format!(".{lib_name}.failed"))
}

pub(super) fn framework_signature(
    include_dirs: &[PathBuf],
    c_flags: &[String],
    cpp_flags: &[String],
) -> String {
    let mut parts = Vec::with_capacity(include_dirs.len() + c_flags.len() + cpp_flags.len() + 2);
    parts.push("i".to_string());
    parts.extend(
        include_dirs
            .iter()
            .map(|p| p.to_string_lossy().into_owned()),
    );
    parts.push("c".to_string());
    parts.extend(c_flags.iter().cloned());
    parts.push("cxx".to_string());
    parts.extend(cpp_flags.iter().cloned());
    parts.join("\x1f")
}

pub(super) fn latest_mtime(paths: &[PathBuf]) -> Result<Option<std::time::SystemTime>> {
    let mut latest = None;
    for path in paths {
        let modified = std::fs::metadata(path)?.modified()?;
        latest = Some(match latest {
            Some(current) if current > modified => current,
            _ => modified,
        });
    }
    Ok(latest)
}

pub(super) fn should_skip_failed_framework_lib(
    marker_path: &Path,
    signature: &str,
    sources: &[PathBuf],
) -> Result<bool> {
    if !marker_path.exists() {
        return Ok(false);
    }

    let marker_text = std::fs::read_to_string(marker_path)?;
    let recorded_signature = marker_text.lines().next().unwrap_or_default();
    if recorded_signature != signature {
        return Ok(false);
    }

    let Some(latest_source_time) = latest_mtime(sources)? else {
        return Ok(false);
    };
    let marker_time = std::fs::metadata(marker_path)?.modified()?;
    Ok(marker_time >= latest_source_time)
}

pub(super) fn record_failed_framework_lib(marker_path: &Path, signature: &str, error: &str) {
    let _ = std::fs::write(marker_path, format!("{signature}\n{error}\n"));
}

pub(super) fn profile_label(profile: fbuild_core::BuildProfile) -> &'static str {
    match profile {
        fbuild_core::BuildProfile::Release => "release",
        fbuild_core::BuildProfile::Quick => "quick",
    }
}

pub(super) fn compile_db_is_current(build_dir: &Path, project_dir: &Path) -> bool {
    let build_copy = build_dir.join("compile_commands.json");
    if !build_copy.exists() {
        return false;
    }
    crate::compile_database::CompileDatabase::expected_output_path(build_dir, project_dir).exists()
}

/// `-fmacro-prefix-map` that shortens the framework's install root in `__FILE__`.
///
/// The core's log macros embed `__FILE__` in flash, so fbuild's long cache path
/// cost bytes PlatformIO's shorter package path did not (FastLED/fbuild#1432).
/// Only macros are remapped: debug info keeps absolute paths for addr2line.
/// `core_dir` is the framework's `cores/<core>` directory.
pub(super) fn framework_macro_prefix_map(core_dir: &Path) -> Option<String> {
    let root = core_dir.parent()?.parent()?;
    Some(format!(
        "-fmacro-prefix-map={}=framework-arduinoespressif32",
        root.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macro_prefix_map_names_the_framework_root() {
        let core_dir = Path::new("/cache/framework-arduinoespressif32/abc/3.3.5/esp32-3.3.5")
            .join("cores")
            .join("esp32");
        let flag = framework_macro_prefix_map(&core_dir).unwrap();
        let root = core_dir.parent().unwrap().parent().unwrap();
        assert_eq!(
            flag,
            format!(
                "-fmacro-prefix-map={}=framework-arduinoespressif32",
                root.display()
            )
        );
    }

    #[test]
    fn macro_prefix_map_needs_a_cores_parent() {
        assert_eq!(framework_macro_prefix_map(Path::new("esp32")), None);
    }
}
