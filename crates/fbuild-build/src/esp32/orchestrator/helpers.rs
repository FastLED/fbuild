//! Helper functions for the ESP32 orchestrator: failure markers, fingerprinting,
//! flag merging, and small utilities used across orchestration phases.

use std::path::{Path, PathBuf};

use fbuild_core::Result;

use crate::flag_overlay::LanguageExtraFlags;

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

/// Apply user build_flags from platformio.ini onto base compiler flags.
///
/// Matches PlatformIO behavior: user flags are appended to common flags,
/// but `-std=` flags replace the existing standard (not stack). `-D` flags are
/// deduplicated by macro name so later values override earlier defaults without
/// tripping GCC redefinition warnings.
pub(super) fn apply_user_flags(base_flags: &[String], user_flags: &[String]) -> Vec<String> {
    let mut result = base_flags.to_vec();
    for flag in user_flags {
        if flag.starts_with("-std=") {
            // Replace any existing -std= flag
            result.retain(|f| !f.starts_with("-std="));
        } else if let Some(define_name) = define_flag_name(flag) {
            // Replace any existing -DNAME / -DNAME=value flag with the same macro name.
            result.retain(|f| define_flag_name(f) != Some(define_name));
        }
        result.push(flag.clone());
    }
    result
}

pub(super) fn apply_overlay_flags(
    base_flags: &[String],
    overlay: &LanguageExtraFlags,
    probe_name: &str,
) -> Vec<String> {
    apply_user_flags(base_flags, &overlay.for_source(Path::new(probe_name)))
}

pub(super) fn define_flag_name(flag: &str) -> Option<&str> {
    let define = flag.strip_prefix("-D")?;
    let name = define
        .split_once('=')
        .map_or(define, |(name, _)| name)
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}
