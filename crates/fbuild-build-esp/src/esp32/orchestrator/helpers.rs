//! Helper functions for the ESP32 orchestrator: failure markers, fingerprinting,
//! and small utilities used across orchestration phases.
//!
//! Flag merging primitives (`apply_user_flags`, `apply_overlay_flags`) used to
//! live here and were shared across ESP32 orchestration phases. They were
//! lifted to `crate::flag_overlay` so the NXP LPC8xx orchestrator (and any
//! future platform that compiles its libraries against the `[env:*] build_flags`
//! overlay) can reach them without depending on `esp32::orchestrator::helpers`.
//! See FastLED/fbuild#587.

use std::path::{Path, PathBuf};

use fbuild_core::Result;

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
