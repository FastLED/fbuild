//! Project-level discovery helpers: include paths, library detection,
//! and platform-config matching.

use std::path::{Path, PathBuf};

/// Add the project's `include/` directory and `lib/` subdirectories to include paths.
///
/// PlatformIO automatically adds these — replicate that behavior.
///
/// All emitted paths are absolute. When `project_dir` is relative, it is first
/// resolved against the current working directory (see `absolute_from_cwd`).
/// This is load-bearing for the compiler step: the zccache path normalizer
/// (`zccache::path_arg_for_compile_cwd`) only strips the compile-cwd prefix
/// from *absolute* include paths and passes relative ones through unchanged.
/// Compiles run with `cwd = <project>/`, so a relative include like
/// `.build/pio/esp32dev/lib/FastLED` would be re-resolved against the
/// already-project-rooted cwd and yield a doubled path that GCC then fails to
/// open (`fatal error: FastLED.h: No such file or directory`). Promoting
/// `project_dir` to absolute up front keeps the include paths stable through
/// the normalize→exec chain. See FastLED/fbuild#303.
pub fn discover_project_includes(project_dir: &Path, include_dirs: &mut Vec<PathBuf>) {
    let project_dir = crate::compiler::absolute_from_cwd(project_dir);

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

#[cfg(test)]
mod tests {
    use super::*;

    /// FastLED/fbuild#303: every emitted include dir must be absolute, even
    /// when `project_dir` is passed in relative form (which is what
    /// `fbuild test-emu .build/pio/<env>` produces). A relative include path
    /// survives `zccache::path_arg_for_compile_cwd` unchanged and then gets
    /// re-resolved against `cwd = <project>/`, producing a doubled-prefix
    /// non-existent path. Promoting to absolute up front breaks the cycle.
    #[test]
    fn includes_are_absolute_for_absolute_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path();
        std::fs::create_dir_all(project.join("lib").join("FastLED")).unwrap();
        std::fs::write(project.join("lib").join("FastLED").join("FastLED.h"), b"").unwrap();
        std::fs::create_dir_all(project.join("include")).unwrap();

        let mut dirs = Vec::new();
        discover_project_includes(project, &mut dirs);

        assert!(!dirs.is_empty(), "should discover include + lib paths");
        for d in &dirs {
            assert!(
                d.is_absolute(),
                "include dir must be absolute, got: {}",
                d.display()
            );
        }
        // The lib's root must be in the list — that's where FastLED.h lives.
        assert!(
            dirs.iter().any(|d| d.ends_with("FastLED")),
            "lib/FastLED root missing from {:?}",
            dirs
        );
    }

    /// Same property, but with a relative `project_dir` — mirrors what the
    /// daemon receives from `fbuild test-emu .build/pio/esp32dev` (the
    /// `PathBuf` is built directly from the request string and never
    /// canonicalized in the handler).
    #[test]
    fn includes_are_absolute_for_relative_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path();
        std::fs::create_dir_all(project.join("lib").join("FastLED")).unwrap();

        // Build a relative path against the current process cwd that points
        // at our tempdir. If the tempdir isn't under cwd (varies by platform
        // and TMPDIR), fall back to the absolute path — the absolutization
        // invariant must still hold either way.
        let cwd = std::env::current_dir().unwrap();
        let relative = match project.strip_prefix(&cwd) {
            Ok(rel) => PathBuf::from(".").join(rel),
            Err(_) => project.to_path_buf(),
        };

        let mut dirs = Vec::new();
        discover_project_includes(&relative, &mut dirs);

        for d in &dirs {
            assert!(
                d.is_absolute(),
                "include dir must be absolute even when project_dir is relative; got: {}",
                d.display()
            );
        }
    }
}
