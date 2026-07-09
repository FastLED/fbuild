//! Generate compile database entries from compiler flags and source lists.

use std::path::{Path, PathBuf};

use crate::flag_overlay::LanguageExtraFlags;

use super::types::CompileEntry;

/// Generate compile database entries for a set of source files.
///
/// # Arguments
/// - `gcc_path` / `gxx_path` — real compiler paths (not cache wrappers)
/// - `c_flags` / `cpp_flags` — language-specific flags
/// - `include_flags` — separate `-I` flags (for ESP32; empty for AVR/Teensy where they're in c/cpp_flags)
/// - `extra_flags` — user/src flags
/// - `sources` — source files to generate entries for
/// - `build_dir` — where object files go (for `-o` path)
/// - `project_dir` — used as the `directory` field
#[allow(clippy::too_many_arguments)]
pub fn generate_entries(
    gcc_path: &Path,
    gxx_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    include_flags: &[String],
    extra_flags: &LanguageExtraFlags,
    sources: &[PathBuf],
    build_dir: &Path,
    project_dir: &Path,
) -> Vec<CompileEntry> {
    let directory = project_dir.to_string_lossy().to_string();

    sources
        .iter()
        .map(|source| {
            let ext = source
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();

            let (compiler, flags) = match ext.as_str() {
                "c" | "s" => (gcc_path, c_flags),
                _ => (gxx_path, cpp_flags),
            };

            let obj = crate::compiler::CompilerBase::object_path(source, build_dir);
            let source_extra_flags = extra_flags.for_source(source);

            let mut arguments = Vec::with_capacity(
                1 + flags.len() + include_flags.len() + source_extra_flags.len() + 4,
            );
            arguments.push(compiler.to_string_lossy().to_string());
            arguments.extend(flags.iter().cloned());
            arguments.extend(include_flags.iter().cloned());
            arguments.extend(source_extra_flags);
            arguments.push("-c".to_string());
            arguments.push(source.to_string_lossy().to_string());
            arguments.push("-o".to_string());
            arguments.push(obj.to_string_lossy().to_string());

            CompileEntry {
                arguments,
                directory: directory.clone(),
                file: source.to_string_lossy().to_string(),
                output: Some(obj.to_string_lossy().to_string()),
            }
        })
        .collect()
}
