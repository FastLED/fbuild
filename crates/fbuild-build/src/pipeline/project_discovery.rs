//! Project-level discovery helpers: include paths, library detection,
//! and platform-config matching.

use std::path::{Path, PathBuf};

/// Add the project's `include/` directory and `lib/` subdirectories to include paths.
///
/// PlatformIO automatically adds these — replicate that behavior.
pub fn discover_project_includes(project_dir: &Path, include_dirs: &mut Vec<PathBuf>) {
    // PlatformIO automatically includes the project's include/ directory
    let include_dir = project_dir.join("include");
    if include_dir.is_dir() {
        include_dirs.push(include_dir);
    }

    // PlatformIO automatically discovers libraries placed in the project's lib/ directory.
    // Each subdirectory is treated as a library — add its root (and src/ if present).
    let local_lib_dir = project_dir.join("lib");
    if local_lib_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&local_lib_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let lib_src = path.join("src");
                    if lib_src.is_dir() {
                        include_dirs.push(lib_src);
                    }
                    // Always add the root too (some libraries have headers at top level)
                    include_dirs.push(path);
                }
            }
        }
    }

    // Project-as-library detection (PlatformIO convention).
    // When a project root contains library.json or library.properties, the project
    // itself is a library and its src/ directory is automatically added to include
    // paths for any sketch built within the project. This allows building example
    // sketches against the library being developed (e.g., FastLED examples).
    let library_json = project_dir.join("library.json");
    let library_props = project_dir.join("library.properties");
    if library_json.exists() || library_props.exists() {
        let project_src = project_dir.join("src");
        if project_src.is_dir() && !include_dirs.contains(&project_src) {
            include_dirs.push(project_src);
        }
    }
}

/// Returns true if the project is a PlatformIO library (has library.json or library.properties).
pub fn is_project_a_library(project_dir: &Path) -> bool {
    project_dir.join("library.json").exists() || project_dir.join("library.properties").exists()
}

/// Check if a project is configured for a specific platform by reading its platformio.ini.
pub fn is_platform_project(
    project_dir: &Path,
    env_name: &str,
    platform: fbuild_core::Platform,
) -> bool {
    let ini_path = project_dir.join("platformio.ini");
    if let Ok(config) = fbuild_config::PlatformIOConfig::from_path(&ini_path) {
        if let Ok(env) = config.get_env_config(env_name) {
            if let Some(platform_str) = env.get("platform") {
                return platform.matches_str(platform_str);
            }
        }
    }
    false
}
