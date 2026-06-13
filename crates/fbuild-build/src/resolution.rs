//! Single entry point for resolving a build's board + platform from a
//! parsed `platformio.ini`.
//!
//! Historically every build-side call site assembled `(board_id, overrides,
//! project_dir)` by hand and called [`BoardConfig::from_board_id_in_project`]
//! independently (see FastLED/fbuild#519). When a new resolution parameter
//! was added — e.g. project-local `boards/<id>.json` discovery in #515/#516 —
//! each site had to be updated by hand, and a missed one silently dropped the
//! feature (the build behaved as if the knob didn't exist).
//!
//! [`ResolutionContext`] carries that context once. New resolution inputs
//! should be threaded through here, not re-plumbed into each consumer.

use fbuild_config::{BoardConfig, PlatformIOConfig};
use fbuild_core::{FbuildError, Platform, Result};
use std::collections::HashMap;
use std::path::Path;

/// All context needed to resolve a board (and its platform) for one
/// `[env:<name>]` section of a project's `platformio.ini`.
pub struct ResolutionContext<'a> {
    /// Project root (the directory containing `platformio.ini`). Used to
    /// discover a project-local `boards/<id>.json` override.
    pub project_dir: &'a Path,
    /// The `[env:<name>]` section to resolve against.
    pub env_name: &'a str,
    /// The already-parsed project config.
    pub config: &'a PlatformIOConfig,
}

impl<'a> ResolutionContext<'a> {
    pub fn new(project_dir: &'a Path, env_name: &'a str, config: &'a PlatformIOConfig) -> Self {
        Self {
            project_dir,
            env_name,
            config,
        }
    }

    /// The `board` id declared in the env section, erroring if absent.
    pub fn board_id(&self) -> Result<String> {
        self.config
            .get_env_config(self.env_name)?
            .get("board")
            .cloned()
            .ok_or_else(|| FbuildError::ConfigError("missing 'board' in environment config".into()))
    }

    /// Resolve the board config, honoring `[env]` board overrides and a
    /// project-local `boards/<id>.json` when the bundled DB has no entry.
    pub fn resolve_board(&self) -> Result<BoardConfig> {
        let board_id = self.board_id()?;
        let overrides = self.config.get_board_overrides(self.env_name)?;
        BoardConfig::from_board_id_in_project(&board_id, &overrides, Some(self.project_dir))
    }

    /// Resolve the [`Platform`] for this env's board.
    pub fn resolve_platform(&self) -> Result<Platform> {
        let board = self.resolve_board()?;
        platform_of(&board, &board.board)
    }
}

/// Determine the [`Platform`] for a bare board id (no `[env]` overrides),
/// honoring a project-local `boards/<id>.json`. This is the lookup used by
/// sites that only know a board string — e.g. `compile_many`'s per-sketch
/// dispatch — and shares the single "could not determine platform" error
/// with [`ResolutionContext::resolve_platform`] (FastLED/fbuild#519).
pub fn platform_for_board(board_id: &str, project_dir: Option<&Path>) -> Result<Platform> {
    let board = BoardConfig::from_board_id_in_project(board_id, &HashMap::new(), project_dir)?;
    platform_of(&board, board_id)
}

/// Shared mapping from a resolved [`BoardConfig`] to its [`Platform`], with a
/// uniform error keyed by `label` (a board id or define) for diagnostics.
fn platform_of(board: &BoardConfig, label: &str) -> Result<Platform> {
    board.platform().ok_or_else(|| {
        FbuildError::ConfigError(format!(
            "could not determine platform for board '{}' (mcu '{}')",
            label, board.mcu
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_project(ini: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("platformio.ini")).unwrap();
        f.write_all(ini.as_bytes()).unwrap();
        dir
    }

    #[test]
    fn resolves_board_and_platform() {
        let dir = write_project("[env:uno]\nplatform = atmelavr\nboard = uno\n");
        let config = PlatformIOConfig::from_path(&dir.path().join("platformio.ini")).unwrap();
        let ctx = ResolutionContext::new(dir.path(), "uno", &config);
        assert_eq!(ctx.board_id().unwrap(), "uno");
        assert_eq!(ctx.resolve_board().unwrap().mcu, "atmega328p");
        assert_eq!(ctx.resolve_platform().unwrap(), Platform::AtmelAvr);
    }

    #[test]
    fn platform_for_board_free_fn_matches_context() {
        assert_eq!(
            super::platform_for_board("uno", None).unwrap(),
            Platform::AtmelAvr
        );
    }

    #[test]
    fn platform_for_board_unknown_errors() {
        assert!(super::platform_for_board("nonexistent_board", None).is_err());
    }

    #[test]
    fn missing_board_errors() {
        let dir = write_project("[env:none]\nplatform = atmelavr\n");
        let config = PlatformIOConfig::from_path(&dir.path().join("platformio.ini")).unwrap();
        let ctx = ResolutionContext::new(dir.path(), "none", &config);
        assert!(ctx.board_id().is_err());
        assert!(ctx.resolve_board().is_err());
    }
}
