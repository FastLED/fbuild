//! Test suite for the compile_database module, split across multiple files
//! to satisfy the per-file LOC gate.

use std::path::{Path, PathBuf};

use crate::flag_overlay::LanguageExtraFlags;

use super::CompileEntry;

/// Test-only shim around `super::generate_entries` that accepts a flat
/// `&[String]` of extra flags (instead of `LanguageExtraFlags`), since
/// most tests only exercise the common-flag path.
#[allow(clippy::too_many_arguments)]
pub(super) fn generate_entries(
    gcc_path: &Path,
    gxx_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    include_flags: &[String],
    extra_flags: &[String],
    sources: &[PathBuf],
    build_dir: &Path,
    project_dir: &Path,
) -> Vec<CompileEntry> {
    super::generate_entries(
        gcc_path,
        gxx_path,
        c_flags,
        cpp_flags,
        include_flags,
        &LanguageExtraFlags {
            common: extra_flags.to_vec(),
            c: Vec::new(),
            cxx: Vec::new(),
            asm: Vec::new(),
        },
        sources,
        build_dir,
        project_dir,
    )
}

mod cache_wrapper;
mod clang;
mod generate;
mod serialization_and_write;
