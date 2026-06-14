//! Native compatibility layer for a constrained subset of PlatformIO `extra_scripts`.
//!
//! Supported shapes are intentionally narrow: `pre:`/`post:` entries, `Import("env")`
//! in PRE/POST scripts, `Import("projenv")` in POST scripts only, and flag/path
//! mutations over the known compiler/linker scopes. Unsupported behavior fails fast
//! with a recommendation to use `--platformio`.

use std::collections::HashMap;
use std::path::Path;

use crate::flag_overlay::{
    absolutize_if_relative, values_to_args, BuildOverlay, LanguageExtraFlags, LinkExtraFlags,
    ScriptRuntimeResult, ScriptScopeState,
};

const HARNESS: &str = include_str!("script_runtime_harness.py");

/// Lite-SCons backend (FastLED/fbuild#553). An opt-in alternative to
/// `script_runtime_harness.py`'s MockEnv shim that adds effectful
/// `Execute`, recorded pre/post actions, middleware, custom targets,
/// `SConscript` recursion, and `ParseFlagsExtended` (joined + space forms).
const LITE_HARNESS: &str = include_str!("lite_scons_harness.py");

/// Returns `true` when the FBUILD_LITE_SCONS env var requests the lite
/// harness instead of MockEnv. Accepts `"1"`, `"true"`, `"yes"` (case
/// insensitive). Anything else — including unset — keeps the legacy
/// MockEnv behaviour the existing call sites depend on.
fn lite_scons_requested() -> bool {
    matches!(
        std::env::var("FBUILD_LITE_SCONS").as_deref().map(str::trim),
        Ok("1" | "true" | "TRUE" | "True" | "yes" | "YES")
    )
}

#[derive(Debug, serde::Serialize)]
struct ScriptRuntimeInput<'a> {
    project_dir: &'a str,
    env_name: &'a str,
    extra_scripts: &'a [String],
    project_options: &'a HashMap<String, String>,
    board_config: HashMap<String, String>,
    platform_name: Option<String>,
    platformio_home: String,
}

pub fn resolve_extra_script_overlay(
    project_dir: &Path,
    env_name: &str,
    config: &fbuild_config::PlatformIOConfig,
) -> fbuild_core::Result<BuildOverlay> {
    resolve_extra_script_overlay_with_mode(project_dir, env_name, config, lite_scons_requested())
}

/// Same as [`resolve_extra_script_overlay`], but with the harness choice
/// passed in explicitly. Lets tests pin the backend deterministically
/// without leaking the `FBUILD_LITE_SCONS` env var across the parallel
/// test runner. See FastLED/fbuild#553.
pub fn resolve_extra_script_overlay_with_mode(
    project_dir: &Path,
    env_name: &str,
    config: &fbuild_config::PlatformIOConfig,
    use_lite_scons: bool,
) -> fbuild_core::Result<BuildOverlay> {
    let extra_scripts = config.get_extra_scripts(env_name)?;
    if extra_scripts.is_empty() {
        return Ok(BuildOverlay::default());
    }

    let project_options = config.get_env_config(env_name)?;
    let board_config = build_script_runtime_board_config(project_options, Some(project_dir))?;
    let input = ScriptRuntimeInput {
        project_dir: &project_dir.to_string_lossy(),
        env_name,
        extra_scripts: &extra_scripts,
        project_options,
        board_config,
        platform_name: project_options.get("platform").cloned(),
        platformio_home: fbuild_paths::get_platformio_home()
            .to_string_lossy()
            .to_string(),
    };

    let python = find_python().ok_or_else(|| {
        fbuild_core::FbuildError::BuildFailed(
            "extra_scripts detected but no Python interpreter was found; \
             install Python or use --platformio"
                .to_string(),
        )
    })?;

    let temp_dir = tempfile::tempdir().map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to create temporary directory for extra_scripts runtime: {}",
            e
        ))
    })?;
    let harness_filename = if use_lite_scons {
        "fbuild_lite_scons_harness.py"
    } else {
        "fbuild_extra_scripts_runtime.py"
    };
    let harness_path = temp_dir.path().join(harness_filename);
    let input_path = temp_dir.path().join("input.json");
    let harness_body = if use_lite_scons {
        LITE_HARNESS
    } else {
        HARNESS
    };
    std::fs::write(&harness_path, harness_body).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to write extra_scripts harness: {}",
            e
        ))
    })?;
    std::fs::write(
        &input_path,
        serde_json::to_vec_pretty(&input).map_err(|e| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to serialize extra_scripts runtime input: {}",
                e
            ))
        })?,
    )
    .map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to write extra_scripts runtime input: {}",
            e
        ))
    })?;

    // Route the spawn through the daemon's containment group so a
    // daemon crash mid-evaluation doesn't leave a python child running
    // in the background. See FastLED/fbuild#32. Uses fbuild_core's
    // NativeProcess-backed runner to drain stdout/stderr concurrently.
    let harness_path_str = harness_path.to_string_lossy();
    let input_path_str = input_path.to_string_lossy();
    let mut argv: Vec<&str> = python.iter().map(|s| s.as_str()).collect();
    argv.push(harness_path_str.as_ref());
    argv.push(input_path_str.as_ref());
    let output = fbuild_core::subprocess::run_command(&argv, Some(project_dir), None, None)
        .map_err(|e| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to run extra_scripts runtime via '{}': {}",
                python.join(" "),
                e
            ))
        })?;

    if !output.success() {
        let stderr = output.stderr.trim();
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "extra_scripts runtime failed: {}\nRecommendation: use --platformio for this project.",
            if stderr.is_empty() {
                format!("process exited with status {}", output.exit_code)
            } else {
                stderr.to_string()
            }
        )));
    }

    let runtime: ScriptRuntimeResult = serde_json::from_str(&output.stdout).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to parse extra_scripts runtime output: {}",
            e
        ))
    })?;

    if !runtime.unsupported.is_empty() {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "unsupported extra_scripts operations detected: {}\nRecommendation: use --platformio for this project.",
            runtime.unsupported.join("; ")
        )));
    }

    let mut overlay = BuildOverlay {
        global_compile: scope_to_compile_overlay(project_dir, &runtime.env)?,
        project_compile: scope_to_compile_overlay(project_dir, &runtime.projenv)?,
        link: scope_to_link_overlay(project_dir, &runtime.env)?,
        notes: runtime.notes,
        // Only populated when the lite harness ran. The MockEnv harness
        // never emits this field, so serde leaves it None for legacy
        // call sites. See FastLED/fbuild#553.
        lite_scons_records: runtime.lite_scons_records,
    };
    // Project-only scope can also contribute link flags in user scripts.
    overlay
        .link
        .extend(&scope_to_link_overlay(project_dir, &runtime.projenv)?);
    Ok(overlay)
}

fn scope_to_compile_overlay(
    project_dir: &Path,
    scope: &ScriptScopeState,
) -> fbuild_core::Result<LanguageExtraFlags> {
    let mut common = cppdefines_to_flags(&scope.cppdefines)?;
    common.extend(cpppath_to_flags(project_dir, &scope.cpppath)?);
    common.extend(values_to_args(&scope.ccflags, project_dir)?);
    Ok(LanguageExtraFlags {
        common,
        c: values_to_args(&scope.cflags, project_dir)?,
        cxx: values_to_args(&scope.cxxflags, project_dir)?,
        asm: values_to_args(&scope.asflags, project_dir)?,
    })
}

fn scope_to_link_overlay(
    project_dir: &Path,
    scope: &ScriptScopeState,
) -> fbuild_core::Result<LinkExtraFlags> {
    let mut flags = values_to_args(&scope.linkflags, project_dir)?;
    for path in &scope.libpath {
        for entry in values_to_args(std::slice::from_ref(path), project_dir)? {
            let resolved = absolutize_if_relative(project_dir, &entry);
            flags.push(format!("-L{}", resolved.display()));
        }
    }
    let libs = libs_to_flags(project_dir, &scope.libs)?;
    Ok(LinkExtraFlags { flags, libs })
}

fn cpppath_to_flags(
    project_dir: &Path,
    values: &[serde_json::Value],
) -> fbuild_core::Result<Vec<String>> {
    let mut flags = Vec::new();
    for entry in values_to_args(values, project_dir)? {
        let resolved = absolutize_if_relative(project_dir, &entry);
        flags.push(format!("-I{}", resolved.display()));
    }
    Ok(flags)
}

fn cppdefines_to_flags(values: &[serde_json::Value]) -> fbuild_core::Result<Vec<String>> {
    let mut flags = Vec::new();
    for value in values {
        match value {
            serde_json::Value::String(s) => flags.push(format!("-D{}", s)),
            serde_json::Value::Object(map) => {
                let kind = map.get("kind").and_then(|v| v.as_str()).ok_or_else(|| {
                    fbuild_core::FbuildError::BuildFailed(
                        "invalid extra_scripts CPPDEFINES entry".to_string(),
                    )
                })?;
                if kind != "kv" {
                    return Err(fbuild_core::FbuildError::BuildFailed(format!(
                        "unsupported CPPDEFINES entry kind '{}'",
                        kind
                    )));
                }
                let key = map.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
                    fbuild_core::FbuildError::BuildFailed(
                        "missing CPPDEFINES key from script runtime".to_string(),
                    )
                })?;
                let value = map.get("value").ok_or_else(|| {
                    fbuild_core::FbuildError::BuildFailed(
                        "missing CPPDEFINES value from script runtime".to_string(),
                    )
                })?;
                let rendered = match value {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => {
                        if *b {
                            "1".to_string()
                        } else {
                            "0".to_string()
                        }
                    }
                    _ => {
                        return Err(fbuild_core::FbuildError::BuildFailed(
                            "unsupported CPPDEFINES value type".to_string(),
                        ))
                    }
                };
                flags.push(format!("-D{}={}", key, rendered));
            }
            _ => {
                return Err(fbuild_core::FbuildError::BuildFailed(
                    "unsupported CPPDEFINES script runtime entry".to_string(),
                ))
            }
        }
    }
    Ok(flags)
}

fn libs_to_flags(
    project_dir: &Path,
    values: &[serde_json::Value],
) -> fbuild_core::Result<Vec<String>> {
    let mut flags = Vec::new();
    for value in values {
        for entry in values_to_args(std::slice::from_ref(value), project_dir)? {
            if entry.starts_with("-l") {
                flags.push(entry);
                continue;
            }
            let looks_like_path = entry.contains(std::path::MAIN_SEPARATOR)
                || entry.contains('/')
                || entry.ends_with(".a")
                || entry.ends_with(".o")
                || entry.ends_with(".lib");
            if looks_like_path {
                let resolved = absolutize_if_relative(project_dir, &entry);
                flags.push(resolved.to_string_lossy().to_string());
            } else {
                flags.push(format!("-l{}", entry));
            }
        }
    }
    Ok(flags)
}

fn find_python() -> Option<Vec<String>> {
    let candidates: &[&[&str]] = if cfg!(windows) {
        &[&["python"], &["py", "-3"]]
    } else {
        &[&["python3"], &["python"]]
    };

    for candidate in candidates {
        let mut argv: Vec<&str> = candidate.to_vec();
        argv.push("--version");
        if let Ok(output) = fbuild_core::subprocess::run_command(&argv, None, None, None) {
            if output.success() {
                return Some(candidate.iter().map(|s| (*s).to_string()).collect());
            }
        }
    }
    None
}

fn build_script_runtime_board_config(
    project_options: &HashMap<String, String>,
    project_dir: Option<&Path>,
) -> fbuild_core::Result<HashMap<String, String>> {
    let mut result = HashMap::new();
    let Some(board_id) = project_options.get("board") else {
        return Ok(result);
    };

    // Shares the same project-local boards/*.json resolution as the
    // pipeline `BuildContext` and `compile_many::platform_for_board` so a
    // user's `<project>/boards/<id>.json` is the single source of truth
    // for every build-side board lookup. Routes through the shared
    // `resolution::resolve_board` funnel (FastLED/fbuild#515, #519).
    let board = crate::resolution::resolve_board(board_id, &HashMap::new(), project_dir)?;
    result.insert("name".to_string(), board.name.clone());
    result.insert("build.mcu".to_string(), board.mcu.clone());
    result.insert("build.f_cpu".to_string(), board.f_cpu.clone());
    result.insert("build.board".to_string(), board.board.clone());
    result.insert("build.core".to_string(), board.core.clone());
    result.insert("build.variant".to_string(), board.variant.clone());
    if let Some(value) = &board.extra_flags {
        result.insert("build.extra_flags".to_string(), value.clone());
    }
    if let Some(value) = &board.flash_mode {
        result.insert("build.flash_mode".to_string(), value.clone());
    }
    if let Some(value) = &board.memory_type {
        result.insert("build.memory_type".to_string(), value.clone());
    }
    if let Some(value) = &board.psram_type {
        result.insert("build.psram_type".to_string(), value.clone());
    }
    if let Some(value) = &board.partitions {
        result.insert("build.partitions".to_string(), value.clone());
    }
    if let Some(value) = &board.ldscript {
        result.insert("build.ldscript".to_string(), value.clone());
    }
    if let Some(value) = &board.platform_str {
        result.insert("platform".to_string(), value.clone());
    }
    if let Some(value) = &board.upload_protocol {
        result.insert("upload.protocol".to_string(), value.clone());
    }
    if let Some(value) = &board.upload_speed {
        result.insert("upload.speed".to_string(), value.clone());
    }
    if let Some(value) = project_options.get("framework") {
        result.insert("frameworks".to_string(), value.clone());
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag_overlay::ScriptScopeState;
    use std::fs;

    fn write_runtime_project(
        extra_scripts: &str,
        script_name: &str,
        script_body: &str,
    ) -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let project_dir = temp.path();
        fs::write(
            project_dir.join("platformio.ini"),
            format!(
                "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
extra_scripts = {}
",
                extra_scripts
            ),
        )
        .unwrap();
        fs::write(project_dir.join(script_name), script_body).unwrap();
        temp
    }

    fn resolve_runtime_error(project_dir: &Path) -> String {
        let config =
            fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))
                .unwrap();
        resolve_extra_script_overlay(project_dir, "demo", &config)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn test_cppdefines_to_flags_string_and_kv() {
        let flags = cppdefines_to_flags(&[
            serde_json::Value::String("FOO".to_string()),
            serde_json::json!({"kind": "kv", "key": "BAR", "value": "baz"}),
            serde_json::json!({"kind": "kv", "key": "COUNT", "value": 7}),
        ])
        .unwrap();
        assert_eq!(flags, vec!["-DFOO", "-DBAR=baz", "-DCOUNT=7"]);
    }

    #[test]
    fn test_libs_to_flags_names_and_paths() {
        let project_dir = Path::new("/tmp/project");
        let flags = libs_to_flags(
            project_dir,
            &[
                serde_json::Value::String("m".to_string()),
                serde_json::Value::String("libs/foo.a".to_string()),
            ],
        )
        .unwrap();
        assert_eq!(flags[0], "-lm");
        assert_eq!(
            flags[1],
            absolutize_if_relative(project_dir, "libs/foo.a")
                .to_string_lossy()
                .to_string()
        );
    }

    #[test]
    fn test_scope_to_compile_overlay_maps_common_and_language_flags() {
        let project_dir = Path::new("/tmp/project");
        let scope = ScriptScopeState {
            cppdefines: vec![
                serde_json::Value::String("FOO".to_string()),
                serde_json::json!({"kind": "kv", "key": "BAR", "value": 1}),
            ],
            cpppath: vec![serde_json::Value::String("include".to_string())],
            ccflags: vec![serde_json::Value::String("-Wall".to_string())],
            cflags: vec![serde_json::Value::String("-std=c11".to_string())],
            cxxflags: vec![serde_json::Value::String("-std=gnu++20".to_string())],
            asflags: vec![serde_json::Value::String("-x".to_string())],
            ..Default::default()
        };

        let overlay = scope_to_compile_overlay(project_dir, &scope).unwrap();
        assert!(overlay.common.contains(&"-DFOO".to_string()));
        assert!(overlay.common.contains(&"-DBAR=1".to_string()));
        assert!(overlay.common.contains(&format!(
            "-I{}",
            absolutize_if_relative(project_dir, "include").display()
        )));
        assert!(overlay.common.contains(&"-Wall".to_string()));
        assert_eq!(overlay.c, vec!["-std=c11"]);
        assert_eq!(overlay.cxx, vec!["-std=gnu++20"]);
        assert_eq!(overlay.asm, vec!["-x"]);
    }

    #[test]
    fn test_scope_to_link_overlay_maps_libpath_and_libs() {
        let project_dir = Path::new("/tmp/project");
        let scope = ScriptScopeState {
            linkflags: vec![serde_json::Value::String("-Wl,--gc-sections".to_string())],
            libpath: vec![serde_json::Value::String("lib".to_string())],
            libs: vec![
                serde_json::Value::String("m".to_string()),
                serde_json::Value::String("archives/foo.a".to_string()),
            ],
            ..Default::default()
        };

        let overlay = scope_to_link_overlay(project_dir, &scope).unwrap();
        assert!(overlay.flags.contains(&"-Wl,--gc-sections".to_string()));
        assert!(overlay.flags.contains(&format!(
            "-L{}",
            absolutize_if_relative(project_dir, "lib").display()
        )));
        assert_eq!(overlay.libs[0], "-lm");
        assert_eq!(
            overlay.libs[1],
            absolutize_if_relative(project_dir, "archives/foo.a")
                .to_string_lossy()
                .to_string()
        );
    }

    #[test]
    fn test_resolve_extra_script_overlay_supports_dump_shim() {
        if find_python().is_none() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let project_dir = temp.path();
        fs::write(
            project_dir.join("platformio.ini"),
            "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
extra_scripts = post:dump_test.py
",
        )
        .unwrap();
        fs::write(
            project_dir.join("dump_test.py"),
            "\
Import(\"env\", \"projenv\")
state = env.Dump()
proj_state = projenv.Dump()
if \"CPPDEFINES\" not in state or \"CPPDEFINES\" not in proj_state:
    raise RuntimeError(\"missing dump scopes\")
env.Append(CPPDEFINES=[\"DUMP_SHIM_OK\"])
",
        )
        .unwrap();

        let config =
            fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))
                .unwrap();
        let overlay = resolve_extra_script_overlay(project_dir, "demo", &config).unwrap();
        assert!(overlay
            .global_compile
            .common
            .contains(&"-DDUMP_SHIM_OK".to_string()));
    }

    #[test]
    fn test_resolve_extra_script_overlay_supports_common_noop_scons_helpers() {
        if find_python().is_none() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let project_dir = temp.path();
        fs::write(
            project_dir.join("platformio.ini"),
            "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
extra_scripts = post:helpers_test.py
",
        )
        .unwrap();
        fs::write(
            project_dir.join("helpers_test.py"),
            "\
Import(\"env\")
if env.IsCleanTarget():
    raise RuntimeError(\"unexpected clean target\")
if env.IsIntegrationDump():
    raise RuntimeError(\"unexpected integration dump\")
flattened = env.Flatten([[\"a\"], [\"b\", [\"c\"]]])
if flattened != [\"a\", \"b\", \"c\"]:
    raise RuntimeError(\"unexpected flatten result\")
env.Execute(env.VerboseAction(\"echo noop\", \"noop\"))
env.Append(CPPDEFINES=[\"HELPERS_SHIM_OK\"])
",
        )
        .unwrap();

        let config =
            fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))
                .unwrap();
        let overlay = resolve_extra_script_overlay(project_dir, "demo", &config).unwrap();
        assert!(overlay
            .global_compile
            .common
            .contains(&"-DHELPERS_SHIM_OK".to_string()));
    }

    #[test]
    fn test_resolve_extra_script_overlay_supports_board_config_shim() {
        if find_python().is_none() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let project_dir = temp.path();
        fs::write(
            project_dir.join("platformio.ini"),
            "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
extra_scripts = post:board_config_test.py
",
        )
        .unwrap();
        fs::write(
            project_dir.join("board_config_test.py"),
            "\
Import(\"env\")
board = env.BoardConfig()
if board.get(\"build.mcu\") != \"atmega328p\":
    raise RuntimeError(\"unexpected board mcu\")
if board.get(\"build.f_cpu\") != \"16000000L\":
    raise RuntimeError(\"unexpected board f_cpu\")
env.Append(CPPDEFINES=[\"BOARD_CONFIG_SHIM_OK\"])
",
        )
        .unwrap();

        let config =
            fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))
                .unwrap();
        let overlay = resolve_extra_script_overlay(project_dir, "demo", &config).unwrap();
        assert!(overlay
            .global_compile
            .common
            .contains(&"-DBOARD_CONFIG_SHIM_OK".to_string()));
    }

    #[test]
    fn test_resolve_extra_script_overlay_supports_pio_platform_shim() {
        if find_python().is_none() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let project_dir = temp.path();
        fs::write(
            project_dir.join("platformio.ini"),
            "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
extra_scripts = post:pio_platform_test.py
",
        )
        .unwrap();
        fs::write(
            project_dir.join("pio_platform_test.py"),
            "\
Import(\"env\")
platform = env.PioPlatform()
if platform.name != \"atmelavr\":
    raise RuntimeError(\"unexpected platform name\")
if not platform.is_embedded():
    raise RuntimeError(\"expected embedded platform\")
pkg = platform.get_package_dir(\"tool-avrdude\")
if not pkg.endswith(\"tool-avrdude\"):
    raise RuntimeError(\"unexpected package path\")
env.Append(CPPDEFINES=[\"PIO_PLATFORM_SHIM_OK\"])
",
        )
        .unwrap();

        let config =
            fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))
                .unwrap();
        let overlay = resolve_extra_script_overlay(project_dir, "demo", &config).unwrap();
        assert!(overlay
            .global_compile
            .common
            .contains(&"-DPIO_PLATFORM_SHIM_OK".to_string()));
    }

    #[test]
    fn test_resolve_extra_script_overlay_rejects_unsupported_import_name() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project(
            "post:bad_import_test.py",
            "bad_import_test.py",
            "\
Import(\"board\")
",
        );
        let err = resolve_runtime_error(temp.path());
        assert!(err.contains("Import('board') is not supported"), "{err}");
        assert!(err.contains("Recommendation: use --platformio"), "{err}");
    }

    #[test]
    fn test_resolve_extra_script_overlay_rejects_projenv_in_pre_script() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project(
            "pre:pre_projenv_test.py",
            "pre_projenv_test.py",
            "\
Import(\"env\", \"projenv\")
",
        );
        let err = resolve_runtime_error(temp.path());
        assert!(
            err.contains("projenv is not available in PRE extra_scripts"),
            "{err}"
        );
        assert!(err.contains("Recommendation: use --platformio"), "{err}");
    }

    #[test]
    fn test_resolve_extra_script_overlay_rejects_unsupported_scope_mutation() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project(
            "post:unsupported_scope_test.py",
            "unsupported_scope_test.py",
            "\
Import(\"env\")
env.Append(FOO=[\"x\"])
",
        );
        let err = resolve_runtime_error(temp.path());
        assert!(
            err.contains("env.append on unsupported scope 'FOO'"),
            "{err}"
        );
        assert!(err.contains("Recommendation: use --platformio"), "{err}");
    }

    #[test]
    fn test_resolve_extra_script_overlay_rejects_unsupported_script_prefix() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project(
            "mid:prefix_test.py",
            "prefix_test.py",
            "\
Import(\"env\")
",
        );
        let err = resolve_runtime_error(temp.path());
        assert!(
            err.contains("unsupported extra_scripts prefix 'mid'"),
            "{err}"
        );
        assert!(err.contains("Recommendation: use --platformio"), "{err}");
    }

    /// Write a project whose `platformio.ini` carries extra `[env:demo]` lines
    /// (e.g. `build_type = debug`) alongside a single `extra_scripts` entry.
    fn write_runtime_project_with_config(
        env_lines: &str,
        extra_scripts: &str,
        script_name: &str,
        script_body: &str,
    ) -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let project_dir = temp.path();
        fs::write(
            project_dir.join("platformio.ini"),
            format!(
                "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
{env_lines}extra_scripts = {extra_scripts}
"
            ),
        )
        .unwrap();
        fs::write(project_dir.join(script_name), script_body).unwrap();
        temp
    }

    fn resolve_runtime_overlay(project_dir: &Path) -> BuildOverlay {
        let config =
            fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))
                .unwrap();
        resolve_extra_script_overlay(project_dir, "demo", &config).unwrap()
    }

    // ---- SIMPLE tier ------------------------------------------------------

    /// Marlin `common-cxxflags.py`-style script: language-specific append,
    /// `GetBuildType()` gating, in-place `BUILD_FLAGS` append, and a no-op
    /// `AddPostAction`. Source: MarlinFirmware/Marlin buildroot scripts.
    #[test]
    fn test_shim_simple_marlin_cxxflags_style() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project_with_config(
            "build_type = debug\n",
            "post:common_cxxflags.py",
            "common_cxxflags.py",
            "\
Import(\"env\")
flags = []
if \"teensy\" not in env[\"PIOENV\"]:
    flags.append(\"-Wno-register\")
env.Append(CXXFLAGS=flags)
if env.GetBuildType() == \"debug\":
    env.Append(CPPDEFINES=[\"MARLIN_DEBUG\"])
env[\"BUILD_FLAGS\"].append(\"-DBOARD_F_CPU=16000000\")
env.AddPostAction(\"$PROGPATH\", lambda *a, **k: None)
",
        );

        let overlay = resolve_runtime_overlay(temp.path());
        assert!(
            overlay
                .global_compile
                .cxx
                .contains(&"-Wno-register".to_string()),
            "{overlay:?}"
        );
        assert!(
            overlay
                .global_compile
                .common
                .contains(&"-DMARLIN_DEBUG".to_string()),
            "{overlay:?}"
        );
        assert!(
            overlay
                .global_compile
                .common
                .contains(&"-DBOARD_F_CPU=16000000".to_string()),
            "BUILD_FLAGS should fold into common compile flags: {overlay:?}"
        );
    }

    /// Tuple-shaped `CPPDEFINES` appended in place via `__getitem__` must still
    /// emit `-Dkey=value`, not a malformed array entry.
    #[test]
    fn test_shim_simple_inplace_tuple_cppdefine() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project_with_config(
            "",
            "post:tuple_define.py",
            "tuple_define.py",
            "\
Import(\"env\")
env[\"CPPDEFINES\"].append((\"VERSION\", 7))
env.Append(CPPDEFINES=[\"PLAIN\"])
",
        );

        let overlay = resolve_runtime_overlay(temp.path());
        assert!(
            overlay
                .global_compile
                .common
                .contains(&"-DVERSION=7".to_string()),
            "{overlay:?}"
        );
        assert!(
            overlay
                .global_compile
                .common
                .contains(&"-DPLAIN".to_string()),
            "{overlay:?}"
        );
    }

    // ---- MEDIUM tier ------------------------------------------------------

    /// namf `platformio_script.py`-style script: obtains env via
    /// `from SCons.Script import DefaultEnvironment`, reads + rewrites
    /// `LINKFLAGS`, and registers a no-op post action.
    #[test]
    fn test_shim_medium_default_environment_linkflags() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project_with_config(
            "",
            "post:namf_style.py",
            "namf_style.py",
            "\
from SCons.Script import DefaultEnvironment
env = DefaultEnvironment()
flags = \" \".join(env[\"LINKFLAGS\"])
flags = flags.replace(\"-u _printf_float\", \"\")
env.Replace(LINKFLAGS=flags.split())
env.Append(LINKFLAGS=[\"-Wl,--gc-sections\"])
def after_build(*a, **k):
    pass
env.AddPostAction(\"$BUILD_DIR/firmware.bin\", after_build)
",
        );

        let overlay = resolve_runtime_overlay(temp.path());
        assert!(
            overlay
                .link
                .flags
                .contains(&"-Wl,--gc-sections".to_string()),
            "{overlay:?}"
        );
    }

    /// m5panel `littlefsbuilder.py`-style script: `env.get()` plus a
    /// `Replace` on a non-flag tool scope. The tool scope must be recorded as
    /// a note (not a hard failure) while the real flag mutation lands.
    #[test]
    fn test_shim_medium_nonflag_scope_recorded_not_rejected() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project_with_config(
            "",
            "post:littlefs.py",
            "littlefs.py",
            "\
Import(\"env\")
env.Replace(MKSPIFFSTOOL=env.get(\"PROJECT_DIR\") + \"/tools/mklittlefs\")
env.Append(CPPDEFINES=[\"LFS_OK\"])
",
        );

        let overlay = resolve_runtime_overlay(temp.path());
        assert!(
            overlay
                .global_compile
                .common
                .contains(&"-DLFS_OK".to_string()),
            "{overlay:?}"
        );
        assert!(
            overlay.notes.iter().any(|n| n.contains("MKSPIFFSTOOL")),
            "non-flag scope should be recorded as a note: {overlay:?}"
        );
    }

    // ---- COMPLEX tier (graceful refusal / no-op) --------------------------

    /// amsreader `generate_includes.py`-style script: a no-op `Execute` of a
    /// `VerboseAction`. The script makes no flag mutations; the runtime must
    /// succeed, collect nothing, and note the ignored action.
    #[test]
    fn test_shim_complex_execute_verbose_action_is_noop() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project_with_config(
            "",
            "post:generate_includes.py",
            "generate_includes.py",
            "\
from SCons.Script import DefaultEnvironment
env = DefaultEnvironment()
env.Execute(env.VerboseAction(\"$PYTHONEXE -m pip install css_html_js_minify\", \"Installing\"))
",
        );

        let overlay = resolve_runtime_overlay(temp.path());
        assert!(
            overlay.global_compile.is_empty() && overlay.project_compile.is_empty(),
            "effectful codegen script should contribute no flags: {overlay:?}"
        );
        assert!(
            overlay.notes.iter().any(|n| n.contains("Execute")),
            "{overlay:?}"
        );
    }

    /// Marlin `common-dependencies.py`-style script: `SConscript` recursion is
    /// structurally unshimmable and must bail with a `--platformio` hint
    /// rather than silently producing wrong flags.
    #[test]
    fn test_shim_complex_sconscript_bails() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project_with_config(
            "",
            "post:common_dependencies.py",
            "common_dependencies.py",
            "\
Import(\"env\")
env.Append(CPPDEFINES=[\"EARLY\"])
env.SConscript(\"feature.py\", exports=\"env\")
",
        );
        let err = resolve_runtime_error(temp.path());
        assert!(err.contains("SConscript is not supported"), "{err}");
        assert!(err.contains("Recommendation: use --platformio"), "{err}");
    }

    // -----------------------------------------------------------------
    // Lite-SCons harness tests (FastLED/fbuild#553)
    // -----------------------------------------------------------------
    //
    // These five tests exercise the five spike patterns (see
    // https://github.com/FastLED/fbuild/issues/553#issuecomment-4702659508)
    // that the MockEnv shim structurally can't model. Each uses
    // `resolve_extra_script_overlay_with_mode(..., true)` so the harness
    // choice doesn't depend on the FBUILD_LITE_SCONS env var leaking
    // across the parallel test runner.

    fn resolve_lite_overlay(project_dir: &Path) -> BuildOverlay {
        let config =
            fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))
                .unwrap();
        resolve_extra_script_overlay_with_mode(project_dir, "demo", &config, true).unwrap()
    }

    /// Effectful `env.Execute(env.Action(callable))`: a generator script
    /// materialises a file on disk at resolve time, and fbuild sees both
    /// the executed-action record AND the generated-files manifest.
    /// MockEnv can't do this — it no-op-records `Execute` and the file
    /// never lands.
    #[test]
    fn test_lite_scons_executes_generator_action() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project(
            "pre:generator.py",
            "generator.py",
            "\
import os
from SCons.Script import Import

Import(\"env\")


def write_buildinfo(target, source, env):
    out = os.path.join(env[\"PROJECT_DIR\"], \"buildinfo.h\")
    with open(out, \"w\", encoding=\"utf-8\") as fh:
        fh.write('#pragma once\\n#define BUILDINFO_RAN 1\\n')
    return 0


env.Execute(env.Action(write_buildinfo, \"Generating buildinfo.h\"))
env.Append(CPPDEFINES=[(\"BUILDINFO_PRESENT\", \"1\")])
",
        );

        let overlay = resolve_lite_overlay(temp.path());
        let records = overlay
            .lite_scons_records
            .as_ref()
            .expect("lite-SCons records must be present when FBUILD_LITE_SCONS path runs");
        assert_eq!(
            records.executed_actions.len(),
            1,
            "Execute(...) must record one executed action: {overlay:?}"
        );
        // Confirm the file was actually materialised on disk — the whole
        // point of effectful Execute is that fbuild sees the generated
        // input before it kicks off the native compile.
        let generated = temp.path().join("buildinfo.h");
        assert!(
            generated.is_file(),
            "generator script must materialise buildinfo.h on disk at {}",
            generated.display()
        );
        assert!(
            records.generated_files.iter().any(|f| f
                .get("path")
                .and_then(|p| p.as_str())
                .is_some_and(|p| p.replace('\\', "/").ends_with("/buildinfo.h"))),
            "generated_files manifest must include buildinfo.h: {:?}",
            records.generated_files
        );
        assert!(
            overlay
                .global_compile
                .common
                .contains(&"-DBUILDINFO_PRESENT=1".to_string()),
            "Append(CPPDEFINES=...) after Execute must still land: {overlay:?}"
        );
    }

    /// `env.AddBuildMiddleware(callback, regex)`: recorded with the
    /// callback's `__name__` and the glob pattern so fbuild can invoke
    /// the callback per matching source file during native compile.
    #[test]
    fn test_lite_scons_records_middleware() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project(
            "post:middleware.py",
            "middleware.py",
            "\
from SCons.Script import Import

Import(\"env\")


def tweak_arduino_core_flags(env, node):
    return env.Object(node, CCFLAGS=env[\"CCFLAGS\"] + [\"-DCORE_BUILD=1\"])


env.AddBuildMiddleware(tweak_arduino_core_flags, \"*ArduinoCore-*/cores/arduino/*.cpp\")
env.Append(CCFLAGS=[\"-Wno-unused-parameter\"])
",
        );

        let overlay = resolve_lite_overlay(temp.path());
        let records = overlay
            .lite_scons_records
            .as_ref()
            .expect("lite-SCons records expected");
        assert_eq!(records.middleware.len(), 1);
        let entry = &records.middleware[0];
        assert_eq!(
            entry.get("callback_repr").and_then(|v| v.as_str()),
            Some("tweak_arduino_core_flags")
        );
        assert_eq!(
            entry.get("regex").and_then(|v| v.as_str()),
            Some("*ArduinoCore-*/cores/arduino/*.cpp")
        );
        assert!(
            overlay
                .global_compile
                .common
                .contains(&"-Wno-unused-parameter".to_string()),
            "ordinary Append still applies alongside middleware: {overlay:?}"
        );
    }

    /// `env.AddPostAction(target, action)`: target template (unresolved)
    /// + callable record so fbuild's native deploy pipeline can run the
    /// callback post-link.
    #[test]
    fn test_lite_scons_records_post_action() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project(
            "post:postaction.py",
            "postaction.py",
            "\
from SCons.Script import Import

Import(\"env\")


def merge_firmware(target, source, env):
    return 0


env.AddPostAction(\"$BUILD_DIR/$PROGNAME$PROGSUFFIX\", merge_firmware)
",
        );

        let overlay = resolve_lite_overlay(temp.path());
        let records = overlay
            .lite_scons_records
            .as_ref()
            .expect("lite-SCons records expected");
        assert_eq!(records.recorded_post_actions.len(), 1);
        let entry = &records.recorded_post_actions[0];
        assert_eq!(
            entry.get("target").and_then(|v| v.as_str()),
            Some("$BUILD_DIR/$PROGNAME$PROGSUFFIX"),
            "target template must be preserved unresolved so fbuild can subst at deploy time"
        );
        assert!(
            entry
                .get("action_repr")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("merge_firmware")),
            "action_repr must identify the callback: {entry:?}"
        );
    }

    /// `env.SConscript(\"child.py\")`: paths resolve relative to the
    /// *calling* script's directory (one of the 3 bugs the spike caught
    /// — see issue comment). Child mutations land on the same env.
    #[test]
    fn test_lite_scons_sconscript_recursion_lands_child_mutations() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project(
            "post:parent.py",
            "parent.py",
            "\
from SCons.Script import Import

Import(\"env\")

env.Append(CPPDEFINES=[\"PARENT_DEFINE\"])
env.SConscript(\"child.py\")
",
        );
        // Drop the child next to the parent so caller-relative resolution
        // picks it up correctly.
        std::fs::write(
            temp.path().join("child.py"),
            "env.Append(CPPDEFINES=[(\"CHILD_DEFINE\", \"1\")])\n",
        )
        .unwrap();

        let overlay = resolve_lite_overlay(temp.path());
        assert!(
            overlay
                .global_compile
                .common
                .contains(&"-DPARENT_DEFINE".to_string()),
            "parent's define must land: {overlay:?}"
        );
        assert!(
            overlay
                .global_compile
                .common
                .contains(&"-DCHILD_DEFINE=1".to_string()),
            "child SConscript's define must land via recursive eval: {overlay:?}"
        );
    }

    /// `env.ParseFlagsExtended(...)` routes both `-Ipath` AND `-I path`
    /// (space-separated) into `CPPPATH`. The space form is the second of
    /// the 3 spike-caught bugs — see issue comment.
    #[test]
    fn test_lite_scons_parseflags_handles_joined_and_space_forms() {
        if find_python().is_none() {
            return;
        }

        let temp = write_runtime_project(
            "post:parseflags.py",
            "parseflags.py",
            "\
from SCons.Script import Import

Import(\"env\")

parsed = env.ParseFlagsExtended(\"-Ijoined/inc -I separated/inc -DKEY=VAL -lextrasym\")
env.Append(**parsed)
",
        );

        let overlay = resolve_lite_overlay(temp.path());
        let common = &overlay.global_compile.common;
        let common_joined = common.join(" ");
        assert!(
            common_joined.contains("joined/inc"),
            "joined -Ipath form must reach CPPPATH (common: {common:?})"
        );
        assert!(
            common_joined.contains("separated/inc"),
            "space-separated -I path form must reach CPPPATH (common: {common:?})"
        );
        assert!(
            common.contains(&"-DKEY=VAL".to_string()),
            "-D KEY=VAL must reach CPPDEFINES (common: {common:?})"
        );
        // Library tokens land on the link side.
        assert!(
            overlay.link.libs.contains(&"-lextrasym".to_string()),
            "-l name must reach link.libs: {:?}",
            overlay.link.libs
        );
    }
}
