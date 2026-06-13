//! `fbuild clangd-config`: emit an IDE-ready clangd configuration for a
//! project's default (or chosen) PlatformIO environment so that "Go to
//! Definition", header hover, and include resolution work in VS Code / clangd
//! without any manual setup.
//!
//! The command sits on top of the existing `compile_database` machinery: it
//! ensures `compile_commands.json` exists (via `build -t compiledb`), reads the
//! real cross-compiler path out of it, and writes `.clangd`,
//! `.vscode/settings.json`, and `.vscode/extensions.json` at the project root.
//! It does not touch the build pipeline.

use super::build::{normalize_path, run_build};

/// Generate clangd / VS Code configuration for the project's default env.
pub async fn run_clangd_config(
    project_dir: String,
    environment: Option<String>,
    verbose: bool,
) -> fbuild_core::Result<()> {
    let project_dir = normalize_path(&project_dir)?;
    let project_path = std::path::Path::new(&project_dir);

    // Step 1: Resolve the environment name (explicit -e wins, else default).
    let env_name = resolve_env_name(project_path, environment)?;
    println!("Using environment: {}", env_name);

    // Step 2: Ensure compile_commands.json exists at the project root.
    let db_path = project_path.join("compile_commands.json");
    if db_path.exists() {
        println!("Using existing compile_commands.json");
    } else {
        println!("Generating compile_commands.json...");
        run_build(
            project_dir.clone(),
            Some(env_name.clone()),
            false, // clean
            verbose,
            None,  // jobs
            false, // quick
            false, // release
            false, // dry_run
            Some("compiledb".to_string()),
            None,
            true, // no_timestamp
            None,
        )
        .await?;
        if !db_path.exists() {
            return Err(fbuild_core::FbuildError::Other(
                "compile_commands.json was not generated".into(),
            ));
        }
    }

    // Step 3: Pull the real cross-compiler path out of the compile database.
    let compiler = extract_compiler_path(&db_path)?;
    let query_driver = compiler_query_driver_glob(&compiler);
    println!("Detected compiler: {}", compiler);

    // Step 4: Write .clangd
    let clangd_path = project_path.join(".clangd");
    std::fs::write(&clangd_path, render_clangd_yaml(&compiler)).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to write {}: {}", clangd_path.display(), e))
    })?;

    // Step 5: Write/merge .vscode/settings.json (only the clangd-related keys).
    let vscode_dir = project_path.join(".vscode");
    std::fs::create_dir_all(&vscode_dir).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to create {}: {}", vscode_dir.display(), e))
    })?;
    let settings_path = vscode_dir.join("settings.json");
    let merged_settings = merge_vscode_settings(&settings_path, &query_driver)?;
    std::fs::write(&settings_path, merged_settings).map_err(|e| {
        fbuild_core::FbuildError::Other(format!(
            "failed to write {}: {}",
            settings_path.display(),
            e
        ))
    })?;

    // Step 6: Write .vscode/extensions.json only if it does not already exist.
    let extensions_path = vscode_dir.join("extensions.json");
    let wrote_extensions = if extensions_path.exists() {
        false
    } else {
        std::fs::write(&extensions_path, render_extensions_json()).map_err(|e| {
            fbuild_core::FbuildError::Other(format!(
                "failed to write {}: {}",
                extensions_path.display(),
                e
            ))
        })?;
        true
    };

    // Step 7: Summary.
    println!("\nWrote clangd configuration:");
    println!("  {}", db_path.display());
    println!("  {}", clangd_path.display());
    println!("  {}", settings_path.display());
    if wrote_extensions {
        println!("  {}", extensions_path.display());
    } else {
        println!(
            "  {} (left unchanged — already exists)",
            extensions_path.display()
        );
    }
    println!("\nInstall the clangd extension (llvm-vs-code-extensions.vscode-clangd),");
    println!("then run \"clangd: Restart language server\" in VS Code to pick up the config.");

    Ok(())
}

/// Resolve the environment name: explicit `-e` wins, otherwise fall back to the
/// project's default environment (PLATFORMIO_DEFAULT_ENVS → `[platformio]
/// default_envs` → first env in file order).
fn resolve_env_name(
    project_path: &std::path::Path,
    environment: Option<String>,
) -> fbuild_core::Result<String> {
    if let Some(env) = environment {
        return Ok(env);
    }
    let ini_path = project_path.join("platformio.ini");
    if !ini_path.exists() {
        return Err(fbuild_core::FbuildError::ConfigError(format!(
            "no platformio.ini found at {}",
            ini_path.display()
        )));
    }
    let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
    config
        .get_default_environment()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError(
                "no environments defined in platformio.ini".into(),
            )
        })
}

/// Extract the absolute cross-compiler path from the first entry of a
/// `compile_commands.json`. The compile database records the real GCC/G++
/// binary (not a cache wrapper) as `arguments[0]`.
fn extract_compiler_path(db_path: &std::path::Path) -> fbuild_core::Result<String> {
    let content = std::fs::read_to_string(db_path).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to read compile_commands.json: {}", e))
    })?;
    let entries: Vec<serde_json::Value> = serde_json::from_str(&content).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to parse compile_commands.json: {}", e))
    })?;
    compiler_from_entries(&entries).ok_or_else(|| {
        fbuild_core::FbuildError::Other(
            "could not determine compiler path from compile_commands.json".into(),
        )
    })
}

/// Pull `arguments[0]` (or the first token of `command`) from the first entry.
fn compiler_from_entries(entries: &[serde_json::Value]) -> Option<String> {
    let entry = entries.first()?;
    if let Some(args) = entry.get("arguments").and_then(|a| a.as_array()) {
        if let Some(first) = args.first().and_then(|v| v.as_str()) {
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    // Fallback: some databases use a single "command" string.
    if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
        if let Some(first) = cmd.split_whitespace().next() {
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    None
}

/// Build a `--query-driver` glob for clangd from a compiler path: the
/// compiler's `bin/` directory plus `/*`, with forward slashes (clangd-friendly
/// on Windows too).
fn compiler_query_driver_glob(compiler: &str) -> String {
    let normalized = compiler.replace('\\', "/");
    let bin_dir = match normalized.rfind('/') {
        Some(idx) => &normalized[..idx],
        None => ".",
    };
    format!("{}/*", bin_dir)
}

/// Render the `.clangd` YAML, pinning the compilation database to the project
/// root and trusting the build's real compiler.
fn render_clangd_yaml(compiler: &str) -> String {
    let compiler_fwd = compiler.replace('\\', "/");
    format!(
        "# Generated by `fbuild clangd-config` — safe to edit, regenerate to refresh.\n\
CompileFlags:\n\
\x20\x20CompilationDatabase: .\n\
\x20\x20# Trust the build's compiler instead of clangd's default driver guess.\n\
\x20\x20Compiler: {compiler}\n\
Diagnostics:\n\
\x20\x20# Many embedded toolchains emit flags clangd cannot parse cleanly.\n\
\x20\x20Suppress: [drv_unknown_argument, unknown-warning-option]\n",
        compiler = compiler_fwd
    )
}

/// Render the recommended-extensions JSON.
fn render_extensions_json() -> String {
    "{\n  \"recommendations\": [\n    \"llvm-vs-code-extensions.vscode-clangd\"\n  ]\n}\n"
        .to_string()
}

/// Merge clangd-related keys into a (possibly pre-existing) `.vscode/settings.json`,
/// preserving any unrelated keys. Only the clangd / MS-extension keys are updated.
fn merge_vscode_settings(
    settings_path: &std::path::Path,
    query_driver: &str,
) -> fbuild_core::Result<String> {
    let mut root: serde_json::Map<String, serde_json::Value> = if settings_path.exists() {
        let content = std::fs::read_to_string(settings_path).map_err(|e| {
            fbuild_core::FbuildError::Other(format!(
                "failed to read {}: {}",
                settings_path.display(),
                e
            ))
        })?;
        // Tolerate an empty/whitespace file as an empty object.
        if content.trim().is_empty() {
            serde_json::Map::new()
        } else {
            serde_json::from_str(&content).map_err(|e| {
                fbuild_core::FbuildError::Other(format!(
                    "failed to parse {} as JSON: {}",
                    settings_path.display(),
                    e
                ))
            })?
        }
    } else {
        serde_json::Map::new()
    };

    root.insert(
        "C_Cpp.intelliSenseEngine".into(),
        serde_json::Value::String("disabled".into()),
    );
    root.insert(
        "C_Cpp.autoAddFileAssociations".into(),
        serde_json::Value::Bool(false),
    );
    root.insert(
        "clangd.arguments".into(),
        serde_json::Value::Array(
            clangd_arguments(query_driver)
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );

    let mut out = serde_json::to_string_pretty(&serde_json::Value::Object(root)).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to serialize settings.json: {}", e))
    })?;
    out.push('\n');
    Ok(out)
}

/// The clangd argument list written into `.vscode/settings.json`.
fn clangd_arguments(query_driver: &str) -> Vec<String> {
    vec![
        "--compile-commands-dir=${workspaceFolder}".to_string(),
        format!("--query-driver={}", query_driver),
        "--background-index".to_string(),
        "--clang-tidy".to_string(),
        "--header-insertion=never".to_string(),
        "--completion-style=detailed".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_from_arguments_first_token() {
        let entries: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"file":"a.cpp","arguments":["/tc/bin/avr-g++","-c","a.cpp"]}]"#,
        )
        .unwrap();
        assert_eq!(
            compiler_from_entries(&entries).as_deref(),
            Some("/tc/bin/avr-g++")
        );
    }

    #[test]
    fn compiler_from_command_string_fallback() {
        let entries: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"file":"a.cpp","command":"/tc/bin/xtensa-esp32-elf-gcc -c a.cpp"}]"#,
        )
        .unwrap();
        assert_eq!(
            compiler_from_entries(&entries).as_deref(),
            Some("/tc/bin/xtensa-esp32-elf-gcc")
        );
    }

    #[test]
    fn query_driver_glob_uses_bin_dir_forward_slashes() {
        assert_eq!(
            compiler_query_driver_glob(r"C:\tc\bin\avr-g++.exe"),
            "C:/tc/bin/*"
        );
        assert_eq!(
            compiler_query_driver_glob("/home/u/.platformio/packages/tc/bin/arm-none-eabi-g++"),
            "/home/u/.platformio/packages/tc/bin/*"
        );
    }

    #[test]
    fn clangd_yaml_mentions_compiler_and_database() {
        let yaml = render_clangd_yaml(r"C:\tc\bin\avr-g++");
        assert!(yaml.contains("CompilationDatabase: ."));
        assert!(yaml.contains("Compiler: C:/tc/bin/avr-g++"));
    }

    #[test]
    fn merge_preserves_unrelated_keys_and_sets_clangd() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"editor.tabSize": 2, "files.trimTrailingWhitespace": true}"#,
        )
        .unwrap();

        let merged = merge_vscode_settings(&settings, "C:/tc/bin/*").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&merged).unwrap();

        // Unrelated keys preserved.
        assert_eq!(parsed["editor.tabSize"], serde_json::json!(2));
        assert_eq!(
            parsed["files.trimTrailingWhitespace"],
            serde_json::json!(true)
        );
        // clangd keys set.
        assert_eq!(
            parsed["C_Cpp.intelliSenseEngine"],
            serde_json::json!("disabled")
        );
        let args = parsed["clangd.arguments"].as_array().unwrap();
        assert!(args
            .iter()
            .any(|a| a.as_str() == Some("--query-driver=C:/tc/bin/*")));
        assert!(args
            .iter()
            .any(|a| a.as_str() == Some("--compile-commands-dir=${workspaceFolder}")));
    }

    #[test]
    fn merge_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");

        let first = merge_vscode_settings(&settings, "C:/tc/bin/*").unwrap();
        std::fs::write(&settings, &first).unwrap();
        let second = merge_vscode_settings(&settings, "C:/tc/bin/*").unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn merge_tolerates_empty_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        std::fs::write(&settings, "   \n").unwrap();
        let merged = merge_vscode_settings(&settings, "C:/tc/bin/*").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(
            parsed["C_Cpp.intelliSenseEngine"],
            serde_json::json!("disabled")
        );
    }
}
