//! VS Code emitter: `.vscode/settings.json` (merge-don't-clobber) and
//! `.vscode/extensions.json` (write-once).

use std::path::Path;

use fbuild_core::path::NormalizedPath;

/// Write/merge `.vscode/settings.json` and (if absent) `.vscode/extensions.json`.
/// Returns `(path, was_written)` pairs for the caller's summary output.
pub(super) fn emit(project_path: &Path) -> fbuild_core::Result<Vec<(NormalizedPath, bool)>> {
    let vscode_dir = project_path.join(".vscode");
    std::fs::create_dir_all(&vscode_dir).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to create {}: {}", vscode_dir.display(), e))
    })?;

    let settings_path = NormalizedPath::from(vscode_dir.join("settings.json"));
    let merged_settings = merge_vscode_settings(&settings_path)?;
    std::fs::write(&settings_path, merged_settings).map_err(|e| {
        fbuild_core::FbuildError::Other(format!(
            "failed to write {}: {}",
            settings_path.display(),
            e
        ))
    })?;

    // Atomic write — FastLED/fbuild#844 bridge pair 6 (state-file write).
    let extensions_path = NormalizedPath::from(vscode_dir.join("extensions.json"));
    let wrote_extensions = if extensions_path.exists() {
        false
    } else {
        fbuild_core::fs::write_atomic_sync(&extensions_path, render_extensions_json()).map_err(
            |e| {
                fbuild_core::FbuildError::Other(format!(
                    "failed to write {}: {}",
                    extensions_path.display(),
                    e
                ))
            },
        )?;
        true
    };

    Ok(vec![
        (settings_path, true),
        (extensions_path, wrote_extensions),
    ])
}

/// Render the recommended-extensions JSON.
fn render_extensions_json() -> String {
    "{\n  \"recommendations\": [\n    \"llvm-vs-code-extensions.vscode-clangd\"\n  ]\n}\n"
        .to_string()
}

/// Merge clangd-related keys into a (possibly pre-existing) `.vscode/settings.json`,
/// preserving any unrelated keys. Only the clangd / MS-extension keys are updated.
fn merge_vscode_settings(settings_path: &Path) -> fbuild_core::Result<String> {
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
            clangd_arguments()
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

/// The clangd argument list written into `.vscode/settings.json`. VS Code
/// (unlike Zed) has a `${workspaceFolder}` variable, so it can point clangd
/// at the compile database explicitly rather than relying solely on
/// `.clangd`'s `CompilationDatabase: .`.
fn clangd_arguments() -> Vec<String> {
    let mut args = vec!["--compile-commands-dir=${workspaceFolder}".to_string()];
    args.extend(super::shared_clangd_arguments());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_unrelated_keys_and_sets_clangd() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"editor.tabSize": 2, "files.trimTrailingWhitespace": true}"#,
        )
        .unwrap();

        let merged = merge_vscode_settings(&settings).unwrap();
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
        assert!(
            args.iter()
                .any(|a| a.as_str() == Some("--compile-commands-dir=${workspaceFolder}"))
        );
        // The degenerate --query-driver argument is gone (FastLED/fbuild#1076
        // Phase 0 — builtin include dirs are baked into the DB instead).
        assert!(
            !args
                .iter()
                .any(|a| a.as_str().is_some_and(|s| s.starts_with("--query-driver")))
        );
    }

    #[test]
    fn merge_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");

        let first = merge_vscode_settings(&settings).unwrap();
        std::fs::write(&settings, &first).unwrap();
        let second = merge_vscode_settings(&settings).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn merge_tolerates_empty_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        std::fs::write(&settings, "   \n").unwrap();
        let merged = merge_vscode_settings(&settings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(
            parsed["C_Cpp.intelliSenseEngine"],
            serde_json::json!("disabled")
        );
    }

    #[test]
    fn emit_writes_settings_and_extensions_into_new_project() {
        let tmp = tempfile::tempdir().unwrap();
        let written = emit(tmp.path()).unwrap();
        assert_eq!(written.len(), 2);
        assert!(written.iter().all(|(_, was_written)| *was_written));
        assert!(tmp.path().join(".vscode/settings.json").exists());
        assert!(tmp.path().join(".vscode/extensions.json").exists());
    }

    #[test]
    fn emit_leaves_existing_extensions_json_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let vscode_dir = tmp.path().join(".vscode");
        std::fs::create_dir_all(&vscode_dir).unwrap();
        std::fs::write(vscode_dir.join("extensions.json"), "{}\n").unwrap();

        let written = emit(tmp.path()).unwrap();
        let extensions_entry = written
            .iter()
            .find(|(p, _)| p.ends_with("extensions.json"))
            .unwrap();
        assert!(!extensions_entry.1);
        assert_eq!(
            std::fs::read_to_string(vscode_dir.join("extensions.json")).unwrap(),
            "{}\n"
        );
    }
}
