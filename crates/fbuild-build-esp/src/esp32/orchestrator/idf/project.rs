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
        return Err(FbuildError::ConfigError(
            "ESP-IDF source layout changed; run fbuild build --clean before adding a src component"
                .into(),
        ));
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
            "file(GLOB_RECURSE app_sources {} {} {})\nidf_component_register(SRCS ${{app_sources}} INCLUDE_DIRS {}",
            cmake_path(&src.join("*.c"))?,
            cmake_path(&src.join("*.cpp"))?,
            cmake_path(&src.join("*.S"))?,
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

/// Seed Kconfig from native defaults/active config, then fbuild's user overlays.
/// IDF owns validation, target-specific defaults and sdkconfig.h generation.
pub(super) fn sdkconfig(
    project: &Path,
    build: &Path,
    mcu: &str,
    env: &HashMap<String, String>,
    flags: &[String],
) -> Result<PathBuf> {
    let mut seed = String::new();
    let defaults = env.get("board_build.sdkconfig_defaults").map_or_else(
        || vec!["sdkconfig.defaults".into()],
        |s| fbuild_core::shell_split::split(s),
    );
    for file in defaults {
        append_file(
            &mut seed,
            &project.join(&file),
            env.contains_key("board_build.sdkconfig_defaults"),
        )?;
    }
    append_file(
        &mut seed,
        &project.join(format!("sdkconfig.defaults.{mcu}")),
        false,
    )?;
    let active = env
        .get("board_build.esp-idf.sdkconfig_path")
        .map_or("sdkconfig", String::as_str);
    append_file(
        &mut seed,
        &project.join(active),
        env.contains_key("board_build.esp-idf.sdkconfig_path"),
    )?;
    append_file(&mut seed, &project.join("sdkconfig.fragment"), false)?;
    if let Some(options) = env.get("board_build.esp-idf.sdkconfig_options") {
        seed.push_str(options);
        seed.push('\n');
    }
    for flag in flags {
        if let Some(config) = flag.strip_prefix("-DCONFIG_") {
            let (name, value) = config.split_once('=').unwrap_or((config, "y"));
            seed.push_str(&format!("CONFIG_{name}={value}\n"));
        }
    }
    if let Some(csv) = env.get("board_build.partitions") {
        let path = project.join(csv).canonicalize()?;
        seed.push_str(&format!(
            "CONFIG_PARTITION_TABLE_CUSTOM=y\nCONFIG_PARTITION_TABLE_CUSTOM_FILENAME=\"{}\"\n",
            path.to_string_lossy().replace('\\', "/")
        ));
    }
    let inputs = build.join("sdkconfig.inputs");
    let output = build.join("sdkconfig");
    if !output.is_file() || std::fs::read_to_string(&inputs).ok().as_deref() != Some(&seed) {
        std::fs::write(&output, &seed)?;
        write_changed(&inputs, &seed)?;
    }
    Ok(output)
}

fn append_file(text: &mut String, path: &Path, required: bool) -> Result<()> {
    match std::fs::read_to_string(path) {
        Ok(value) => {
            text.push_str(&value);
            text.push('\n');
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
