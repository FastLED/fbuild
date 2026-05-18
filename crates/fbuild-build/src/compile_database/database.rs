//! File IO and library-project detection for `CompileDatabase`.

use std::path::{Path, PathBuf};

use fbuild_core::Result;

use super::types::CompileDatabase;

impl CompileDatabase {
    /// Write `compile_commands.json` to the given directory.
    pub fn write(&self, dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(dir).map_err(|e| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to create directory {}: {}",
                dir.display(),
                e
            ))
        })?;

        let path = dir.join("compile_commands.json");
        let json = serde_json::to_string_pretty(&self.entries).map_err(|e| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to serialize compile database: {}",
                e
            ))
        })?;

        write_if_changed(&path, json.as_bytes())?;

        Ok(path)
    }

    /// Write to `build_dir` and copy to `project_dir` (matching Python fbuild behavior).
    ///
    /// If the project has a `library.json` at its root (indicating it IS a library,
    /// e.g. FastLED), the copy to project root is suppressed to avoid overwriting
    /// a meson/cmake-generated `compile_commands.json` that uses correct source paths.
    pub fn write_and_copy(&self, build_dir: &Path, project_dir: &Path) -> Result<PathBuf> {
        let build_path = self.write(build_dir)?;

        if is_library_project(project_dir) {
            tracing::info!(
                "library.json detected — skipping compile_commands.json copy to project root"
            );
            return Ok(build_path);
        }

        let project_path = self.write(project_dir)?;
        Ok(project_path)
    }

    /// Path callers should report as the effective compile database output.
    pub fn expected_output_path(build_dir: &Path, project_dir: &Path) -> PathBuf {
        if is_library_project(project_dir) {
            build_dir.join("compile_commands.json")
        } else {
            project_dir.join("compile_commands.json")
        }
    }
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if let Ok(existing) = std::fs::read(path) {
        if existing == contents {
            return Ok(());
        }
    }

    std::fs::write(path, contents).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!("failed to write {}: {}", path.display(), e))
    })
}

/// Check if a project is a library (has `library.json` at the root).
///
/// Library projects (e.g. FastLED) often have their own build system that
/// generates a correct `compile_commands.json`. We skip overwriting the
/// project root file to avoid clobbering it.
pub fn is_library_project(project_dir: &Path) -> bool {
    project_dir.join("library.json").exists()
}
