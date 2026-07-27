//! Zed emitter: `.zed/settings.json` (merge-don't-clobber), first cut for
//! the Phase 1 `fbuild ide` MVP on stock Zed (FastLED/fbuild#1076).
//!
//! Zed has no `${workspaceFolder}`-style variable in `lsp` config (unlike VS
//! Code), so the clangd arguments emitted here omit
//! `--compile-commands-dir` entirely and rely on the shared `.clangd` file's
//! `CompilationDatabase: .` — which is editor-neutral and always resolves
//! relative to wherever clangd's working directory is (the project root
//! Zed launches it from).

use std::path::{Path, PathBuf};

/// Write/merge `.zed/settings.json`. Returns `(path, was_written)` pairs
/// for the caller's summary output — always `true` here since the merge
/// itself is the "write" (there is no separate write-once file like VS
/// Code's `extensions.json`).
pub(super) fn emit(project_path: &Path) -> fbuild_core::Result<Vec<(PathBuf, bool)>> {
    let zed_dir = project_path.join(".zed");
    std::fs::create_dir_all(&zed_dir).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to create {}: {}", zed_dir.display(), e))
    })?;

    let settings_path = zed_dir.join("settings.json");
    let merged = merge_zed_settings(&settings_path)?;
    std::fs::write(&settings_path, merged).map_err(|e| {
        fbuild_core::FbuildError::Other(format!(
            "failed to write {}: {}",
            settings_path.display(),
            e
        ))
    })?;

    Ok(vec![(settings_path, true)])
}

/// Merge `file_types` (map `.ino` to the C++ language so clangd starts on
/// sketch buffers) and `lsp.clangd.binary.arguments` into a (possibly
/// pre-existing) `.zed/settings.json`, preserving every unrelated key —
/// including other extensions already mapped under `"C++"` and other
/// languages under `file_types`.
fn merge_zed_settings(settings_path: &Path) -> fbuild_core::Result<String> {
    let mut root: serde_json::Map<String, serde_json::Value> = if settings_path.exists() {
        let content = std::fs::read_to_string(settings_path).map_err(|e| {
            fbuild_core::FbuildError::Other(format!(
                "failed to read {}: {}",
                settings_path.display(),
                e
            ))
        })?;
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

    merge_ino_file_type(&mut root);
    merge_clangd_lsp_config(&mut root);

    let mut out = serde_json::to_string_pretty(&serde_json::Value::Object(root)).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to serialize settings.json: {}", e))
    })?;
    out.push('\n');
    Ok(out)
}

/// Ensure `file_types."C++"` contains `"ino"`, without disturbing any other
/// extensions already mapped there or any other language's `file_types` entry.
fn merge_ino_file_type(root: &mut serde_json::Map<String, serde_json::Value>) {
    let file_types = root
        .entry("file_types")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(file_types_obj) = file_types.as_object_mut() else {
        // Pre-existing value under "file_types" isn't an object — don't
        // clobber whatever the user has there.
        return;
    };

    let cpp_extensions = file_types_obj
        .entry("C++")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(cpp_array) = cpp_extensions.as_array_mut() else {
        return;
    };

    let has_ino = cpp_array.iter().any(|v| v.as_str() == Some("ino"));
    if !has_ino {
        cpp_array.push(serde_json::Value::String("ino".to_string()));
    }
}

/// Set `lsp.clangd.binary.arguments`, preserving any other `lsp.*` server
/// config and any other keys under `lsp.clangd` (e.g. a user-set
/// `binary.path`).
fn merge_clangd_lsp_config(root: &mut serde_json::Map<String, serde_json::Value>) {
    let lsp = root
        .entry("lsp")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(lsp_obj) = lsp.as_object_mut() else {
        return;
    };

    let clangd = lsp_obj
        .entry("clangd")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(clangd_obj) = clangd.as_object_mut() else {
        return;
    };

    let binary = clangd_obj
        .entry("binary")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(binary_obj) = binary.as_object_mut() else {
        return;
    };

    binary_obj.insert(
        "arguments".to_string(),
        serde_json::Value::Array(
            super::shared_clangd_arguments()
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_unrelated_keys_and_sets_ino_and_clangd() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"vim_mode": true, "file_types": {"YAML": ["yml"]}}"#,
        )
        .unwrap();

        let merged = merge_zed_settings(&settings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&merged).unwrap();

        // Unrelated top-level key preserved.
        assert_eq!(parsed["vim_mode"], serde_json::json!(true));
        // Unrelated file_types entry preserved.
        assert_eq!(parsed["file_types"]["YAML"], serde_json::json!(["yml"]));
        // "ino" mapped to C++.
        assert_eq!(parsed["file_types"]["C++"], serde_json::json!(["ino"]));
        // clangd arguments set, without --compile-commands-dir (no Zed
        // ${workspaceFolder} variable to use).
        let args = parsed["lsp"]["clangd"]["binary"]["arguments"]
            .as_array()
            .unwrap();
        assert!(
            args.iter()
                .any(|a| a.as_str() == Some("--background-index"))
        );
        assert!(
            !args
                .iter()
                .any(|a| a.as_str().is_some_and(|s| s.contains("workspaceFolder")))
        );
    }

    #[test]
    fn merge_does_not_duplicate_existing_ino_mapping() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        std::fs::write(&settings, r#"{"file_types": {"C++": ["ino", "tpp"]}}"#).unwrap();

        let merged = merge_zed_settings(&settings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&merged).unwrap();
        let cpp = parsed["file_types"]["C++"].as_array().unwrap();
        assert_eq!(cpp.len(), 2);
        assert!(cpp.contains(&serde_json::json!("ino")));
        assert!(cpp.contains(&serde_json::json!("tpp")));
    }

    #[test]
    fn merge_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");

        let first = merge_zed_settings(&settings).unwrap();
        std::fs::write(&settings, &first).unwrap();
        let second = merge_zed_settings(&settings).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn merge_tolerates_empty_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        std::fs::write(&settings, "   \n").unwrap();
        let merged = merge_zed_settings(&settings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(parsed["file_types"]["C++"], serde_json::json!(["ino"]));
    }

    #[test]
    fn emit_creates_zed_dir_and_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let written = emit(tmp.path()).unwrap();
        assert_eq!(written.len(), 1);
        assert!(written[0].1);
        assert!(tmp.path().join(".zed/settings.json").exists());
    }
}
