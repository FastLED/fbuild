//! Prepare a build-local project only when the user has no root CMake project.

use std::collections::HashMap;
use std::path::Path;

use fbuild_core::path::NormalizedPath;

use fbuild_core::{FbuildError, Result};

/// Parse only the leading line-comment region; retain Kconfig text verbatim.
pub(super) fn parse_sdkconfig_block(
    text: &str,
    path: &Path,
) -> Result<Option<Vec<(usize, String)>>> {
    let mut start = None;
    let mut complete = false;
    let mut body = Vec::new();
    let mut keys = std::collections::HashSet::new();
    let error =
        |line, message| FbuildError::ConfigError(format!("{}:{line}: {message}", path.display()));
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim_start();
        let Some(comment) = trimmed.strip_prefix("//") else {
            if start.is_some() && !complete {
                return Err(error(line, "expected // sdkconfig body line"));
            }
            if trimmed.is_empty() {
                continue;
            }
            break;
        };
        let value = comment.strip_prefix(' ').unwrap_or(comment);
        if value.trim() == "@sdkconfig" {
            if start.is_some() {
                return Err(error(line, "multiple @sdkconfig blocks"));
            }
            start = Some(line);
        } else if start.is_some() && !complete {
            if value.trim() == "@end-sdkconfig" {
                complete = true;
                continue;
            }
            if let Some(key) = block_config_key(value) {
                if !keys.insert(key.to_owned()) {
                    return Err(error(line, "duplicate sdkconfig key"));
                }
            } else if !value.trim().is_empty() && !value.starts_with('#') {
                return Err(error(line, "invalid sdkconfig body line"));
            }
            body.push((line, value.to_owned()));
        }
    }
    if let Some(line) = start {
        if !complete {
            return Err(error(line, "unterminated @sdkconfig block"));
        }
        Ok(Some(body))
    } else {
        Ok(None)
    }
}

fn block_config_key(value: &str) -> Option<&str> {
    let key = if let Some(unset) = value.strip_prefix("# ") {
        unset.strip_suffix(" is not set")?
    } else {
        value.split_once('=')?.0
    };
    let name = key.strip_prefix("CONFIG_")?;
    (!name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_'))
    .then_some(key)
}

#[cfg(test)]
mod block_tests {
    use super::*;

    #[test]
    fn sdkconfig_block_preserves_lines_and_values() {
        let path = Path::new("main.c");
        let text = "// heading\n// @sdkconfig\n// CONFIG_X=1000\n// # CONFIG_Y is not set\n// @end-sdkconfig\nint x;\n";
        assert_eq!(
            parse_sdkconfig_block(text, path).unwrap().unwrap(),
            vec![
                (3, "CONFIG_X=1000".into()),
                (4, "# CONFIG_Y is not set".into())
            ]
        );
        assert!(
            parse_sdkconfig_block("int x;\n// @sdkconfig\n// invalid\n", path)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn sdkconfig_block_errors_name_source_line() {
        for (text, line) in [
            (
                "// @sdkconfig\n// CONFIG_X=y\n// CONFIG_X=n\n// @end-sdkconfig\n",
                3,
            ),
            (
                "// @sdkconfig\n// CONFIG_X=y\n// # CONFIG_X is not set\n// @end-sdkconfig\n",
                3,
            ),
            ("// @sdkconfig\n// CONFIG_X=y\n", 1),
            ("// @sdkconfig\n// invalid\n// @end-sdkconfig\n", 2),
            ("// @sdkconfig\nint x;\n", 2),
            ("// @sdkconfig\n// @end-sdkconfig\n// @sdkconfig\n", 3),
        ] {
            let error = parse_sdkconfig_block(text, Path::new("main.c"))
                .unwrap_err()
                .to_string();
            assert!(error.contains(&format!("main.c:{line}")), "{error}");
        }
    }
}

pub(super) fn write_changed(path: &Path, text: &str) -> Result<()> {
    if std::fs::read_to_string(path).ok().as_deref() != Some(text) {
        std::fs::write(path, text)?;
    }
    Ok(())
}

/// CMake bracket arguments preserve spaces, quotes and semicolons in paths.
pub(super) fn cmake_path(path: &Path) -> Result<String> {
    let value = NormalizedPath::new(path).display_slash();
    if value.contains("]=]") || value.contains(';') {
        return Err(FbuildError::ConfigError(format!(
            "unsupported CMake path: {value}"
        )));
    }
    Ok(format!("[=[{value}]=]"))
}

pub(super) fn source_root(project: &Path, src: &Path, build: &Path) -> Result<NormalizedPath> {
    if project.join("CMakeLists.txt").is_file() {
        return Ok(NormalizedPath::new(project));
    }
    if !src.is_dir() || src == project {
        return Err(FbuildError::ConfigError(format!(
            "IDF requires a source directory or a native root CMakeLists.txt: {}",
            src.display()
        )));
    }
    let root = build.join("project");
    std::fs::create_dir_all(&root)?;
    if src.join("CMakeLists.txt").is_file() && root.join("main/CMakeLists.txt").is_file() {
        std::fs::remove_dir_all(root.join("main"))?;
    }
    let mut cmake = "cmake_minimum_required(VERSION 3.16)\n".to_string();
    if project.join("components").is_dir() {
        cmake.push_str(&format!(
            "list(APPEND EXTRA_COMPONENT_DIRS {})\n",
            cmake_path(&project.join("components"))?
        ));
    }
    if src.join("CMakeLists.txt").is_file() {
        cmake.push_str(&format!(
            "list(APPEND EXTRA_COMPONENT_DIRS {})\n",
            cmake_path(src)?
        ));
    } else {
        let main = root.join("main");
        std::fs::create_dir_all(&main)?;
        let mut component = format!(
            "set(app_glob_options)\nif(NOT CMAKE_SCRIPT_MODE_FILE)\n  list(APPEND app_glob_options CONFIGURE_DEPENDS)\nendif()\nfile(GLOB_RECURSE app_sources ${{app_glob_options}} {} {} {} {} {})\nidf_component_register(SRCS ${{app_sources}} INCLUDE_DIRS {}",
            cmake_path(&src.join("*.c"))?,
            cmake_path(&src.join("*.cpp"))?,
            cmake_path(&src.join("*.S"))?,
            cmake_path(&src.join("*.cc"))?,
            cmake_path(&src.join("*.cxx"))?,
            cmake_path(src)?,
        );
        if project.join("include").is_dir() {
            component.push_str(&format!(" {}", cmake_path(&project.join("include"))?));
        }
        component.push_str(")\n");
        write_changed(&main.join("CMakeLists.txt"), &component)?;
    }
    cmake.push_str("include($ENV{IDF_PATH}/tools/cmake/project.cmake)\nproject(firmware)\n");
    write_changed(&root.join("CMakeLists.txt"), &cmake)?;
    Ok(root.into())
}

/// IDF owns active configuration and defaults-only-for-unset Kconfig semantics.
pub(super) fn sdkconfig(
    project: &Path,
    build: &Path,
    env_name: &str,
    env: &HashMap<String, String>,
) -> Result<Vec<String>> {
    let active = env.get("board_build.esp-idf.sdkconfig_path").map_or_else(
        || project.join(format!("sdkconfig.{env_name}")),
        |path| project.join(path),
    );
    let mut defaults = Vec::new();
    let native = project.join("sdkconfig.defaults");
    if native.is_file() {
        defaults.push(native.to_string_lossy().into_owned());
    }
    if let Some(csv) = env.get("board_build.partitions") {
        let path = project.join(csv).canonicalize()?;
        let overlay = build.join("partitions.defaults");
        write_changed(
            &overlay,
            &format!(
                "CONFIG_PARTITION_TABLE_CUSTOM=y\nCONFIG_PARTITION_TABLE_CUSTOM_FILENAME=\"{}\"\n",
                NormalizedPath::new(path).display_slash()
            ),
        )?;
        defaults.push(overlay.to_string_lossy().into_owned());
    }
    Ok(vec![
        format!("-DSDKCONFIG={}", active.display()),
        format!("-DSDKCONFIG_DEFAULTS={}", defaults.join(";")),
    ])
}

/// Pass board revision defaults before the project's native Kconfig defaults.
pub(super) fn board_sdkconfig(
    params: &crate::BuildParams,
    build: &Path,
    env: &HashMap<String, String>,
    board: &fbuild_config::BoardConfig,
) -> Result<Vec<String>> {
    let mut options = sdkconfig(&params.project_dir, build, &params.env_name, env)?;
    if let Some(defaults) = revision_defaults(board, build)? {
        options[1] = format!(
            "-DSDKCONFIG_DEFAULTS={};{}",
            defaults.display(),
            options[1].trim_start_matches("-DSDKCONFIG_DEFAULTS=")
        );
    }
    Ok(options)
}

/// Translate the board registry's ROM family into IDF's native Kconfig defaults.
pub(super) fn revision_defaults(
    board: &fbuild_config::BoardConfig,
    build: &Path,
) -> Result<Option<NormalizedPath>> {
    let Some(revision) = super::packages::revision(board)? else {
        return Ok(None);
    };
    let path = build.join("board-revision.defaults");
    write_changed(&path, &revision.defaults)?;
    Ok(Some(path.into()))
}

/// Active sdkconfig wins over defaults in IDF; refuse a conflicting ROM family.
pub(super) fn validate_revision(board: &fbuild_config::BoardConfig, build: &Path) -> Result<()> {
    if let Some(revision) = super::packages::revision(board)? {
        let config = std::fs::read_to_string(build.join("config/sdkconfig.h"))?;
        let selected = config
            .lines()
            .any(|line| line == format!("#define {} 1", revision.selector));
        if selected != revision.enabled {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "ESP-IDF sdkconfig selects a silicon revision incompatible with board variant {}; update native {} and revision-minimum settings",
                board.sdk_variant(),
                revision.selector
            )));
        }
    }
    Ok(())
}
