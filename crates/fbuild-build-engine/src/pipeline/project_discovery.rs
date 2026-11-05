//! Project-level discovery helpers: include paths, library detection,
//! and platform-config matching.

use std::path::{Path, PathBuf};

/// Add project and library include roots using fallible manifest-aware discovery.
///
/// Project-as-library examples receive the actual manifest source root, including
/// custom srcDir. Invalid metadata propagates before compiler construction.
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
pub fn discover_project_includes(
    project_dir: &Path,
    include_dirs: &mut Vec<PathBuf>,
) -> fbuild_core::Result<()> {
    let project_dir = crate::compiler::absolute_from_cwd(project_dir);
    let include_dir = project_dir.join("include");
    if include_dir.is_dir() && !include_dirs.contains(&include_dir) {
        include_dirs.push(include_dir);
    }

    // Use the same fallible manifest-aware roots as actual library compilation.
    let local_lib_dir = project_dir.join("lib");
    if local_lib_dir.is_dir() {
        for entry in std::fs::read_dir(&local_lib_dir)? {
            let path = entry?.path();
            if path.is_dir() {
                super::add_extra_library_include_dirs(std::slice::from_ref(&path), include_dirs)?;
            }
        }
    }
    if is_project_a_library(&project_dir) {
        super::add_extra_library_include_dirs(std::slice::from_ref(&project_dir), include_dirs)?;
    }
    Ok(())
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
        discover_project_includes(project, &mut dirs).unwrap();

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
        discover_project_includes(&relative, &mut dirs).unwrap();

        for d in &dirs {
            assert!(
                d.is_absolute(),
                "include dir must be absolute even when project_dir is relative; got: {}",
                d.display()
            );
        }
    }
}

#[cfg(test)]
mod manifest_example_include_tests {
    #[test]
    fn project_library_custom_source_root_reaches_example_compiler_flags() {
        let temp = tempfile::tempdir().unwrap();
        let code = temp.path().join("code");
        let example = temp.path().join("examples/Blink");
        std::fs::create_dir_all(&code).unwrap();
        std::fs::create_dir_all(&example).unwrap();
        std::fs::write(
            temp.path().join("library.json"),
            r#"{"name":"Actual","build":{"srcDir":"code"}}"#,
        )
        .unwrap();
        std::fs::write(code.join("Actual.h"), "int actual();\n").unwrap();
        std::fs::write(
            code.join("actual.cpp"),
            "#include <Actual.h>\nint actual(){return 1;}\n",
        )
        .unwrap();
        std::fs::write(
            example.join("main.cpp"),
            "#include <Actual.h>\nint main(){return actual();}\n",
        )
        .unwrap();
        let mut includes = vec![example];
        // This is the shared call made before real platform compiler creation.
        super::discover_project_includes(temp.path(), &mut includes).unwrap();
        let compiler = crate::compiler::CompilerBase {
            mcu: "ci1306".into(),
            f_cpu: "246000000L".into(),
            defines: std::collections::HashMap::new(),
            include_dirs: includes,
            verbose: false,
        };
        assert!(
            compiler
                .build_include_flags()
                .contains(&format!("-I{}", code.display())),
            "actual example compiler flags must expose project manifest source root"
        );
    }
}

#[cfg(test)]
mod project_manifest_discovery_error_tests {
    #[test]
    fn example_compiler_include_discovery_propagates_invalid_project_manifest() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("library.json"),
            r#"{"build":{"srcDir":"missing-code"}}"#,
        )
        .unwrap();
        assert!(super::discover_project_includes(temp.path(), &mut Vec::new()).is_err());
    }
}
