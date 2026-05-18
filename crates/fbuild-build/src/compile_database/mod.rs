//! Compile database (`compile_commands.json`) generation for IDE support.
//!
//! Generates a JSON compilation database (clangd-compatible) so that
//! "Go to Definition" and other IDE features work with real include paths
//! instead of response file (`@file`) references.

mod cache_wrapper;
mod clang;
mod database;
mod generate;
mod types;

pub use cache_wrapper::strip_cache_wrapper;
pub use clang::translate_flags_for_clang;
pub use database::is_library_project;
pub use generate::generate_entries;
pub use types::{CompileDatabase, CompileEntry, TargetArchitecture};

#[cfg(test)]
mod tests;
