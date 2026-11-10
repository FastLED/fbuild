//! Publishing the delegated build's compile database (FastLED/fbuild#1973).
//!
//! `idf.py` writes its own `compile_commands.json` into the delegated build
//! directory, rooted in scratch paths, and fbuild documents **one** database
//! location for a project — [`CompileDatabase::expected_output_path`], the same
//! path the native path publishes to. This module is the join between the two:
//! it re-roots the delegated entries, merges them with fbuild's own database
//! when there is one, and writes the result at the documented path.
//!
//! fbuild's own database is the one fbuild's compile-database generator wrote
//! for the same environment (`<build>/compile_commands.json`); a project that
//! has an Arduino environment and an `espidf` one therefore keeps the entries
//! fbuild compiled itself. When the two paths coincide (a library project,
//! whose documented path *is* the build directory), the file on disk cannot be
//! told apart from this build's own previous output, so the delegated entries
//! are authoritative there and the stale duplicates are dropped — otherwise a
//! changed `sdkconfig` would keep replaying last build's flags (FastLED/fbuild#1973).

use fbuild_core::path::{NormalizedPath, normalize_for_key};
use fbuild_core::{FbuildError, Result};

use crate::compile_database::{
    CompileDatabase, DatabaseEntry, merge, read_database, reroot, write_database,
};

/// What was published, for the build log and the artifact index.
#[derive(Debug)]
pub(super) struct PublishedDatabase {
    pub path: NormalizedPath,
    pub digest: String,
    pub entries: usize,
    /// Entries fbuild itself compiled, kept from its own database.
    pub from_fbuild: usize,
    /// Entries re-rooted from the delegated build's database.
    pub from_delegated: usize,
}

/// Re-root and merge this build's database, then write it at the documented
/// path. `build_dir` is fbuild's own build directory for the environment (the
/// parent of the delegated root), `project_dir` the project it belongs to.
pub(super) fn publish(
    delegated_build_dir: &NormalizedPath,
    build_dir: &NormalizedPath,
    project_dir: &NormalizedPath,
) -> Result<PublishedDatabase> {
    let delegated_path = delegated_build_dir.join("compile_commands.json");
    let delegated = read_database(&delegated_path)?;
    if delegated.is_empty() {
        return Err(FbuildError::Other(format!(
            "the delegated ESP-IDF build wrote no compile-database entries to {delegated_path}; \
             a CMake/component build compiles at least one translation unit"
        )));
    }
    let delegated = reroot(delegated, project_dir);

    let output = NormalizedPath::new(CompileDatabase::expected_output_path(
        build_dir.as_path(),
        project_dir.as_path(),
    ));
    let native_source = build_dir.join("compile_commands.json");
    let native = native_database(&native_source, &output, &delegated)?;
    let from_fbuild = native.len();

    let merged = merge(&native, delegated);
    let from_delegated = merged.len() - from_fbuild;
    let digest = write_database(&output, &merged)?;
    Ok(PublishedDatabase {
        path: output,
        digest,
        entries: merged.len(),
        from_fbuild,
        from_delegated,
    })
}

/// fbuild's own entries, or none when there is nothing to merge.
///
/// Absent is normal (the first delegated build of a project), and so is the
/// alias case described in the module documentation: there the file *is* the
/// output, so any entry this build also produced is dropped and only the
/// entries fbuild compiled stay.
fn native_database(
    source: &NormalizedPath,
    output: &NormalizedPath,
    delegated: &[DatabaseEntry],
) -> Result<Vec<DatabaseEntry>> {
    if !source.as_path().is_file() {
        return Ok(Vec::new());
    }
    let existing = read_database(source)?;
    if normalize_for_key(source.as_path()) != normalize_for_key(output.as_path()) {
        return Ok(existing);
    }
    let produced: std::collections::BTreeSet<String> = delegated
        .iter()
        .map(DatabaseEntry::translation_unit_key)
        .collect();
    Ok(existing
        .into_iter()
        .filter(|entry| !produced.contains(&entry.translation_unit_key()))
        .collect())
}

#[cfg(test)]
#[path = "espidf_compile_db_tests.rs"]
mod espidf_compile_db_tests;
