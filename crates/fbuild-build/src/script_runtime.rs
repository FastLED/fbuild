//! Native compatibility layer for PlatformIO `extra_scripts`.
//!
//! fbuild evaluates `extra_scripts` via a Python subprocess sidecar that
//! interprets a working subset of SCons against the project's `[env]` —
//! see [`crate::flag_overlay::BuildOverlay`] for what flows back into the
//! native compile/link/deploy pipeline. The harness lives at
//! `lite_scons_harness.py`; see that file's docstring for the API surface.
//!
//! What this backend deliberately does NOT model — single-pass instead of
//! a real DAG, no scanner-driven header dep discovery, no
//! PlatformIO-defined chip-family builders without a native fbuild
//! equivalent — falls through to the `--platformio` passthrough.
//!
//! History: the original MockEnv shim (`script_runtime_harness.py`) lived
//! alongside the lite-SCons harness behind `FBUILD_LITE_SCONS=0` from #581
//! through #583. It was retired in step 4 of the [#553 plan](https://github.com/FastLED/fbuild/issues/553)
//! once lite-SCons proved a functional superset.

use std::collections::HashMap;
use std::path::Path;

use crate::flag_overlay::{
    absolutize_if_relative, values_to_args, BuildOverlay, LanguageExtraFlags, LinkExtraFlags,
    ScriptRuntimeResult, ScriptScopeState,
};

const HARNESS: &str = include_str!("lite_scons_harness.py");

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

pub async fn resolve_extra_script_overlay(
    project_dir: &Path,
    env_name: &str,
    config: &fbuild_config::PlatformIOConfig,
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

    let python = find_python().await.ok_or_else(|| {
        fbuild_core::FbuildError::BuildFailed(
            "extra_scripts detected but no Python interpreter was found; \
             install Python or use --platformio"
                .to_string(),
        )
    })?;

    // FastLED/fbuild#844 (bridge pair 10): rooted under
    // `~/.fbuild/{dev|prod}/tmp/script-runtime/` so the harness sidecar
    // is reachable from a single user-visible directory.
    let temp_dir =
        tempfile::tempdir_in(fbuild_paths::temp_subdir("script-runtime")).map_err(|e| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to create temporary directory for extra_scripts runtime: {}",
                e
            ))
        })?;
    let harness_path = temp_dir.path().join("fbuild_lite_scons_harness.py");
    let input_path = temp_dir.path().join("input.json");
    std::fs::write(&harness_path, HARNESS).map_err(|e| {
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
    // FastLED/fbuild#809: user-supplied `extra_scripts` Python harness
    // — config-time evaluation, never legitimately long. Bound to 60s
    // so a buggy or hostile script cannot wedge the daemon's build
    // pipeline indefinitely.
    let output = fbuild_core::subprocess::run_command(
        &argv,
        Some(project_dir),
        None,
        Some(std::time::Duration::from_secs(60)),
    )
    .await
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

pub(crate) async fn find_python() -> Option<Vec<String>> {
    let candidates: &[&[&str]] = if cfg!(windows) {
        &[&["python"], &["py", "-3"]]
    } else {
        &[&["python3"], &["python"]]
    };

    for candidate in candidates {
        let mut argv: Vec<&str> = candidate.to_vec();
        argv.push("--version");
        // FastLED/fbuild#809: `python --version` on the startup path —
        // bound tightly so a hung interpreter cannot wedge build init.
        if let Ok(output) = fbuild_core::subprocess::run_command(
            &argv,
            None,
            None,
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        {
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

// The `#[cfg(test)] mod tests { ... }` body lives in
// `script_runtime_tests.rs` so this file stays under the repo's
// 1000-LOC gate. The split is purely organisational — tests still
// see `super::*` and the crate-internal types via the `use` lines
// at the top of the tests file.
#[cfg(test)]
#[path = "script_runtime_tests.rs"]
mod tests;
