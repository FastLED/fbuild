use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use crate::flag_overlay::{
    absolutize_if_relative, values_to_args, BuildOverlay, LanguageExtraFlags, LinkExtraFlags,
    ScriptRuntimeResult, ScriptScopeState,
};

const HARNESS: &str = include_str!("script_runtime_harness.py");

#[derive(Debug, serde::Serialize)]
struct ScriptRuntimeInput<'a> {
    project_dir: &'a str,
    env_name: &'a str,
    extra_scripts: &'a [String],
    project_options: &'a HashMap<String, String>,
}

pub fn resolve_extra_script_overlay(
    project_dir: &Path,
    env_name: &str,
    config: &fbuild_config::PlatformIOConfig,
) -> fbuild_core::Result<BuildOverlay> {
    let extra_scripts = config.get_extra_scripts(env_name)?;
    if extra_scripts.is_empty() {
        return Ok(BuildOverlay::default());
    }

    let project_options = config.get_env_config(env_name)?;
    let input = ScriptRuntimeInput {
        project_dir: &project_dir.to_string_lossy(),
        env_name,
        extra_scripts: &extra_scripts,
        project_options,
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
    let harness_path = temp_dir.path().join("fbuild_extra_scripts_runtime.py");
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

    let mut command = Command::new(&python[0]);
    if python.len() > 1 {
        command.args(&python[1..]);
    }
    command
        .arg(&harness_path)
        .arg(&input_path)
        .current_dir(project_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = command.output().map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to run extra_scripts runtime via '{}': {}",
            python.join(" "),
            e
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "extra_scripts runtime failed: {}\nRecommendation: use --platformio for this project.",
            if stderr.is_empty() {
                format!("process exited with status {}", output.status)
            } else {
                stderr
            }
        )));
    }

    let runtime: ScriptRuntimeResult = serde_json::from_slice(&output.stdout).map_err(|e| {
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
        let mut command = Command::new(candidate[0]);
        if candidate.len() > 1 {
            command.args(&candidate[1..]);
        }
        if let Ok(output) = command.arg("--version").output() {
            if output.status.success() {
                return Some(candidate.iter().map(|s| (*s).to_string()).collect());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag_overlay::ScriptScopeState;

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
}
