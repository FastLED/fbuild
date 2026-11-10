//! Compile database (`compile_commands.json`) generation for IDE support.
//!
//! Generates a JSON compilation database (clangd-compatible) so that
//! "Go to Definition" and other IDE features work with real include paths
//! instead of response file (`@file`) references.
//!
//! [`merge`] is the other half: a delegated (CMake/`idf.py`) build's database is
//! re-rooted and merged into fbuild's own, one entry per translation unit, so a
//! project has one database at `CompileDatabase::expected_output_path`
//! (FastLED/fbuild#1973).

mod cache_wrapper;
mod clang;
mod database;
mod generate;
mod merge;
mod types;

pub use cache_wrapper::strip_cache_wrapper;
pub use clang::translate_flags_for_clang;
pub use database::is_library_project;
pub use generate::generate_entries;
pub use merge::{
    DatabaseEntry, Invocation, merge, native_entries, read_database, reroot, write_database,
};
pub use types::{CompileDatabase, CompileEntry, TargetArchitecture};

#[cfg(test)]
mod tests;
