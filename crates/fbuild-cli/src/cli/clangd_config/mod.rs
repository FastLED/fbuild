//! `fbuild clangd-config`: emit an IDE-ready clangd configuration for a
//! project's default (or chosen) PlatformIO environment so that "Go to
//! Definition", header hover, and include resolution work in clangd-backed
//! editors without any manual setup.
//!
//! The command sits on top of the existing `compile_database` machinery: it
//! ensures `compile_commands.json` exists (via `build -t compiledb`) and
//! writes the editor-neutral `.clangd` file plus per-editor project config
//! (`.vscode/*` or `.zed/*`). It does not touch the build pipeline.
//!
//! ## Editor-neutral core + per-editor emitters (FastLED/fbuild#1076 Phase 0)
//!
//! This module is split so the core (env resolution, compile-DB
//! freshness, `.clangd` emission) is reusable by any future editor and by
//! the planned `fbuild ide` command:
//!
//! - `mod.rs` (this file) — `Editor` selection, `ensure_compile_db`,
//!   `emit_clangd_file`, `emit_editor_config` dispatch. These three
//!   `pub(crate)` functions are the reusable core surface.
//! - `vscode` — the VS Code emitter (`.vscode/settings.json`,
//!   `.vscode/extensions.json`). This is the original (and default)
//!   behavior of `fbuild clangd-config`.
//! - `zed` — the Zed emitter (`.zed/settings.json`), first cut for the
//!   Phase 1 `fbuild ide` MVP on stock Zed.
//!
//! `.clangd` itself is intentionally emitted once, here, and shared between
//! editors — `CompilationDatabase: .` plus diagnostic suppression is
//! editor-neutral. It no longer pins a `Compiler:` path or asks clangd to
//! `--query-driver` one: the cross-compiler's builtin include dirs are now
//! baked into `compile_commands.json` itself as `-isystem` args by
//! `CompileDatabase::translate_for_clang` (the query-driver path degenerated
//! to a no-op glob once `translate_for_clang` started rewriting
//! `arguments[0]` to bare `clang`/`clang++` — see
//! `crates/fbuild-build-engine/src/compile_database/clang.rs`).

mod vscode;
mod zed;

use fbuild_core::path::NormalizedPath;

use crate::output;

use super::build::{normalize_path, run_build};

/// Which editor to emit per-editor project config for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Editor {
    VsCode,
    Zed,
}

impl Editor {
    /// Parse a `--editor` value. Callers gate the accepted strings via
    /// clap's `value_parser = ["vscode", "zed"]`, so this always succeeds
    /// for CLI-originated input; the fallback exists so a stray internal
    /// caller degrades to the (safe, original) default instead of panicking.
    fn parse(value: &str) -> Editor {
        match value {
            "zed" => Editor::Zed,
            _ => Editor::VsCode,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Editor::VsCode => "VS Code",
            Editor::Zed => "Zed",
        }
    }
}

/// Generate clangd / editor configuration for the project's default env.
pub async fn run_clangd_config(
    project_dir: String,
    environment: Option<String>,
    verbose: bool,
    editor: String,
    refresh: bool,
) -> fbuild_core::Result<()> {
    let editor = Editor::parse(&editor);
    let project_dir = normalize_path(&project_dir).await?;
    let project_path = std::path::Path::new(&project_dir);

    // Step 1: Resolve the environment name (explicit -e wins, else default).
    let env_name = resolve_env_name(project_path, environment)?;
    output::progress(format!("Using environment: {}", env_name));

    // Step 2: Ensure compile_commands.json exists (and is fresh) at the
    // project root.
    let db_path =
        ensure_compile_db(&project_dir, project_path, &env_name, verbose, refresh).await?;

    // Step 3: Write the shared, editor-neutral .clangd file.
    let clangd_path = emit_clangd_file(project_path)?;

    // Step 4: Write the per-editor project config.
    let editor_paths = emit_editor_config(editor, project_path)?;

    // Step 5: Summary.
    output::result("\nWrote clangd configuration:");
    output::result(format!("  {}", db_path.display()));
    output::result(format!("  {}", clangd_path.display()));
    for (path, written) in &editor_paths {
        if *written {
            output::result(format!("  {}", path.display()));
        } else {
            output::result(format!(
                "  {} (left unchanged — already exists)",
                path.display()
            ));
        }
    }
    output::result(format!(
        "\nInstall the clangd extension for {}, then restart its language server to pick up the config.",
        editor.label()
    ));

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

/// Ensure `compile_commands.json` exists at the project root, regenerating
/// it via `fbuild build -t compiledb` when missing — or, with
/// `refresh: true`, unconditionally (FastLED/fbuild#1076 Phase 0 item 3:
/// "regeneration must be a first-class cheap operation" that the future
/// `fbuild ide` module can call on open / env-switch / after a build).
///
/// `pub(crate)` so the planned `fbuild ide` module (FastLED/fbuild#1076
/// Phase 1) can reuse it directly.
pub(crate) async fn ensure_compile_db(
    project_dir: &str,
    project_path: &std::path::Path,
    env_name: &str,
    verbose: bool,
    refresh: bool,
) -> fbuild_core::Result<NormalizedPath> {
    let db_path = NormalizedPath::from(project_path.join("compile_commands.json"));
    if !refresh && db_path.exists() {
        output::progress("Using existing compile_commands.json");
        return Ok(db_path);
    }

    output::progress("Generating compile_commands.json...");
    run_build(
        project_dir.to_string(),
        Some(env_name.to_string()),
        false, // clean
        false, // clean_all
        verbose,
        None,  // jobs
        false, // quick
        false, // release
        false, // dry_run
        Some("compiledb".to_string()),
        None,
        true, // no_timestamp
        None,
        false, // bloat_analysis
    )
    .await?;
    if !db_path.exists() {
        return Err(fbuild_core::FbuildError::Other(
            "compile_commands.json was not generated".into(),
        ));
    }
    Ok(db_path)
}

/// Write the shared, editor-neutral `.clangd` file. `pub(crate)` so the
/// future `fbuild ide` module can reuse it directly.
pub(crate) fn emit_clangd_file(
    project_path: &std::path::Path,
) -> fbuild_core::Result<NormalizedPath> {
    let clangd_path = NormalizedPath::from(project_path.join(".clangd"));
    std::fs::write(&clangd_path, render_clangd_yaml()).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to write {}: {}", clangd_path.display(), e))
    })?;
    Ok(clangd_path)
}

/// Render the `.clangd` YAML, pinning the compilation database to the
/// project root. No longer pins a `Compiler:` path or requests
/// `--query-driver` (FastLED/fbuild#1076 Phase 0 — the toolchain's builtin
/// include dirs are baked into `compile_commands.json` as `-isystem`
/// instead; the query-driver path had already silently degenerated to a
/// no-op glob).
fn render_clangd_yaml() -> String {
    "# Generated by `fbuild clangd-config` — safe to edit, regenerate to refresh.\n\
CompileFlags:\n\
\x20\x20CompilationDatabase: .\n\
Diagnostics:\n\
\x20\x20# Many embedded toolchains emit flags clangd cannot parse cleanly.\n\
\x20\x20Suppress: [drv_unknown_argument, unknown-warning-option]\n"
        .to_string()
}

/// clangd arguments common to every editor. VS Code additionally prepends
/// `--compile-commands-dir=${workspaceFolder}`; Zed has no such variable and
/// relies on `.clangd`'s `CompilationDatabase: .` instead (editor-neutral,
/// works from any working directory Zed launches clangd in).
pub(crate) fn shared_clangd_arguments() -> Vec<String> {
    vec![
        "--background-index".to_string(),
        "--clang-tidy".to_string(),
        "--header-insertion=never".to_string(),
        "--completion-style=detailed".to_string(),
    ]
}

/// Dispatch to the per-editor emitter. Returns `(path, was_written)` pairs
/// for the summary output — `was_written` is `false` for files intentionally
/// left untouched (e.g. `.vscode/extensions.json` when it already exists).
/// `pub(crate)` so the future `fbuild ide` module can reuse it directly.
pub(crate) fn emit_editor_config(
    editor: Editor,
    project_path: &std::path::Path,
) -> fbuild_core::Result<Vec<(NormalizedPath, bool)>> {
    match editor {
        Editor::VsCode => vscode::emit(project_path),
        Editor::Zed => zed::emit(project_path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_parse_recognizes_zed_and_defaults_to_vscode() {
        assert_eq!(Editor::parse("zed"), Editor::Zed);
        assert_eq!(Editor::parse("vscode"), Editor::VsCode);
        assert_eq!(Editor::parse("anything-else"), Editor::VsCode);
    }

    #[test]
    fn clangd_yaml_no_longer_pins_compiler_or_query_driver() {
        let yaml = render_clangd_yaml();
        assert!(yaml.contains("CompilationDatabase: ."));
        assert!(!yaml.contains("Compiler:"));
        assert!(!yaml.contains("query-driver"));
    }

    #[test]
    fn shared_clangd_arguments_have_no_workspace_folder_variable() {
        // Zed has no `${workspaceFolder}`-style variable — only VS Code's
        // emitter may add `--compile-commands-dir=${workspaceFolder}`.
        assert!(
            shared_clangd_arguments()
                .iter()
                .all(|a| !a.contains("${workspaceFolder}"))
        );
    }
}
