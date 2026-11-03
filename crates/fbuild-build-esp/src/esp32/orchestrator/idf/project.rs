//! Prepare a build-local project only when the user has no root CMake project.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::{FbuildError, Result};

pub(super) fn write_changed(path: &Path, text: &str) -> Result<()> {
    if std::fs::read_to_string(path).ok().as_deref() != Some(text) {
        std::fs::write(path, text)?;
    }
    Ok(())
}

/// CMake bracket arguments preserve spaces, quotes and semicolons in paths.
pub(super) fn cmake_path(path: &Path) -> Result<String> {
    let value = path.to_string_lossy().replace('\\', "/");
    if value.contains("]=]") || value.contains(';') {
        return Err(FbuildError::ConfigError(format!(
            "unsupported CMake path: {value}"
        )));
    }
    Ok(format!("[=[{value}]=]"))
}

pub(super) fn source_root(project: &Path, src: &Path, build: &Path) -> Result<PathBuf> {
    if project.join("CMakeLists.txt").is_file() {
        return Ok(project.to_path_buf());
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
    Ok(root)
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
                path.to_string_lossy().replace('\\', "/")
            ),
        )?;
        defaults.push(overlay.to_string_lossy().into_owned());
    }
    Ok(vec![
        format!("-DSDKCONFIG={}", active.display()),
        format!("-DSDKCONFIG_DEFAULTS={}", defaults.join(";")),
    ])
}
