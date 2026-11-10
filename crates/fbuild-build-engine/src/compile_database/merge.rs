//! Merging a delegated (CMake/`idf.py`) compile database into fbuild's own
//! (FastLED/fbuild#1973).
//!
//! A delegated ESP-IDF build emits a database of its own: measured, 7.39 MB
//! and 959 entries for a two-component application. Two things make it
//! unusable downstream as it lands on disk:
//!
//! - **It is rooted in scratch paths.** `directory` is the build directory the
//!   delegate passed to `idf.py -B`, and framework entries name sources inside
//!   the IDF checkout under the fbuild cache. Neither spelling means anything
//!   to an indexer, an IDE or a later build, and both change when the cache
//!   moves.
//! - **It would overwrite fbuild's own database.** fbuild publishes the
//!   database at [`CompileDatabase::expected_output_path`], the path every
//!   consumer of the native path already reads. A project with both an Arduino
//!   environment and an `espidf` one must not lose the entries fbuild compiled
//!   itself just because a delegated build ran.
//!
//! So this module re-roots a delegated database and merges it into fbuild's,
//! with one entry per translation unit:
//!
//! - fbuild's entry wins where both produced one — it records the compiler
//!   fbuild actually drove, which is the spelling every other fbuild artifact
//!   agrees with.
//! - the delegated entry fills in everything only IDF compiled: the framework,
//!   the bootloader, the components `idf_component_register()` pulled in.
//!
//! The result is deterministic: entries are keyed by the normalized absolute
//! source path, emitted in that key order, and the invocation payload
//! (`command` *or* `arguments`) is carried over byte-for-byte rather than
//! re-split, so two identical builds produce a byte-identical file and a
//! digest that does not churn between them.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use fbuild_core::path::{NormalizedPath, normalize_for_key};
use fbuild_core::{FbuildError, Result};

/// How one entry invokes the compiler: the two spellings the JSON compilation
/// database specification allows.
///
/// Both are kept verbatim. A CMake-generated database uses `command` (one
/// shell-quoted string) and fbuild's generator uses `arguments`; re-splitting
/// the former into the latter would have to re-implement shell quoting, and a
/// mis-split is a silently wrong entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(untagged)]
pub enum Invocation {
    Arguments { arguments: Vec<String> },
    Command { command: String },
}

/// One compile-database entry as it is written to disk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct DatabaseEntry {
    pub directory: String,
    pub file: String,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(flatten)]
    pub invocation: Invocation,
}

impl DatabaseEntry {
    /// The entry as JSON, with the field order the specification's examples
    /// use (`arguments`/`command`, `directory`, `file`, `output`).
    ///
    /// `serde_json::Map` is a `BTreeMap` in this workspace, so the keys are
    /// already alphabetical — which is exactly that order. Building the object
    /// here keeps every entry's byte spelling independent of how it was read.
    fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        match &self.invocation {
            Invocation::Arguments { arguments } => {
                map.insert(
                    "arguments".to_string(),
                    serde_json::Value::from(arguments.clone()),
                );
            }
            Invocation::Command { command } => {
                map.insert(
                    "command".to_string(),
                    serde_json::Value::from(command.clone()),
                );
            }
        }
        map.insert(
            "directory".to_string(),
            serde_json::Value::from(self.directory.clone()),
        );
        map.insert(
            "file".to_string(),
            serde_json::Value::from(self.file.clone()),
        );
        if let Some(output) = &self.output {
            map.insert(
                "output".to_string(),
                serde_json::Value::from(output.clone()),
            );
        }
        serde_json::Value::Object(map)
    }

    /// The translation unit this entry compiles, as a case-folded absolute
    /// path.
    ///
    /// `file` is resolved against `directory` when it is relative, so an entry
    /// that was re-rooted and one that was not still collide on the same key:
    /// that is what makes "one entry per translation unit" true across the two
    /// spellings.
    pub fn translation_unit_key(&self) -> String {
        let file = Path::new(&self.file);
        if file.is_absolute() {
            return normalize_for_key(file);
        }
        normalize_for_key(&Path::new(&self.directory).join(file))
    }
}

/// fbuild's own entries, converted to the on-disk form.
pub fn native_entries(native: &super::types::CompileDatabase) -> Vec<DatabaseEntry> {
    native
        .entries
        .iter()
        .map(|entry| DatabaseEntry {
            directory: entry.directory.clone(),
            file: entry.file.clone(),
            output: entry.output.clone(),
            invocation: Invocation::Arguments {
                arguments: entry.arguments.clone(),
            },
        })
        .collect()
}

/// Parse a database file, in either invocation spelling.
pub fn read_database(path: &NormalizedPath) -> Result<Vec<DatabaseEntry>> {
    let raw = std::fs::read(path.as_path()).map_err(|error| {
        FbuildError::Other(format!("compile database {path} is unreadable: {error}"))
    })?;
    let entries: Vec<DatabaseEntry> = serde_json::from_slice(&raw).map_err(|error| {
        FbuildError::Other(format!(
            "compile database {path} is not a JSON compilation database: {error}"
        ))
    })?;
    Ok(entries)
}

/// Re-root `entries` onto `project_dir`: the one directory fbuild's own
/// spelling uses.
///
/// `directory` becomes the project directory and `file`/`output` are spelled
/// relative to it, so no scratch build-directory prefix and no IDF-checkout
/// prefix survives in either field. A source outside the project keeps a `..`
/// spelling for the same reason — it still resolves, and it no longer embeds
/// an absolute cache location. When the two sides do not share a root (a
/// Windows drive split) the absolute spelling is kept, because no relative
/// path exists. `command`/`arguments` is never touched: it is the replayable
/// payload, and only it may name the real toolchain and flag paths.
pub fn reroot(entries: Vec<DatabaseEntry>, project_dir: &NormalizedPath) -> Vec<DatabaseEntry> {
    entries
        .into_iter()
        .map(|entry| {
            let file = respell(&entry.file, project_dir).unwrap_or(entry.file);
            let output = entry
                .output
                .as_ref()
                .map(|output| respell(output, project_dir).unwrap_or_else(|| output.clone()));
            DatabaseEntry {
                directory: project_dir.display_slash(),
                file,
                output,
                invocation: entry.invocation,
            }
        })
        .collect()
}

/// `path` spelled relative to `project_dir`, or `None` when it already is
/// relative or the two do not share a root.
fn respell(path: &str, project_dir: &NormalizedPath) -> Option<String> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return None;
    }
    relative_spelling(path, project_dir.as_path())
}

/// `path` relative to `base`, without touching the filesystem.
///
/// Returns `None` when the two share no root component (different Windows
/// drives): there is no relative spelling then, and inventing one would be a
/// path that silently does not resolve.
fn relative_spelling(path: &Path, base: &Path) -> Option<String> {
    let path_key = normalize_for_key(path);
    let base_key = normalize_for_key(base);
    let path: Vec<Component> = Path::new(&path_key).components().collect();
    let base: Vec<Component> = Path::new(&base_key).components().collect();
    let common = path
        .iter()
        .zip(base.iter())
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }
    // Climb out of `base` and back down into `path`; a shared root is what
    // `common >= 1` above established, so every `..` here stays inside one
    // filesystem root.
    let mut parts: Vec<String> = vec!["..".to_string(); base.len() - common];
    parts.extend(
        path[common..]
            .iter()
            .map(|component| component.as_os_str().to_string_lossy().into_owned()),
    );
    Some(parts.join("/"))
}

/// Merge `delegated` into `native`, one entry per translation unit.
///
/// fbuild's entry wins an overlap (see the module documentation); the
/// delegated entry is only used where fbuild produced nothing. The output is
/// ordered by translation-unit key, so it does not depend on the order either
/// input arrived in.
pub fn merge(native: &[DatabaseEntry], delegated: Vec<DatabaseEntry>) -> Vec<DatabaseEntry> {
    let mut merged: BTreeMap<String, DatabaseEntry> = BTreeMap::new();
    for entry in delegated {
        merged.entry(entry.translation_unit_key()).or_insert(entry);
    }
    for entry in native {
        merged.insert(entry.translation_unit_key(), entry.clone());
    }
    merged.into_values().collect()
}

/// Serialize `entries` deterministically and write them to `path`.
///
/// Returns the SHA-256 of the bytes written, which is what a caller records to
/// prove two identical builds produced the same database.
pub fn write_database(path: &NormalizedPath, entries: &[DatabaseEntry]) -> Result<String> {
    let json: Vec<serde_json::Value> = entries.iter().map(DatabaseEntry::to_json).collect();
    let text = serde_json::to_string_pretty(&json)
        .map_err(|error| FbuildError::Other(format!("compile database serialization: {error}")))?;
    let bytes = text.into_bytes();
    let digest = sha256_hex(&bytes);
    write_atomically(path, &bytes)?;
    Ok(digest)
}

/// Write `bytes` to `path` only when they differ, so a warm rebuild does not
/// touch the file's mtime and invalidate a downstream indexer for nothing.
fn write_atomically(path: &NormalizedPath, bytes: &[u8]) -> Result<()> {
    if std::fs::read(path.as_path()).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    fbuild_core::fs::write_atomic_sync(path.as_path(), bytes)
        .map_err(|error| FbuildError::Other(format!("writing {path} failed: {error}")))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;

    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod merge_tests;
