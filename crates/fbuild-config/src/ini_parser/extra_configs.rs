//! `[platformio] extra_configs`: merge the declared INI files over the base.
//!
//! PlatformIO extends `platformio.ini` with the files listed in
//! `[platformio] extra_configs`
//! (<https://docs.platformio.org/en/latest/projectconf/sections/platformio/options/generic/extra_configs.html>):
//! shell-style wildcards are allowed, the files are merged in declaration
//! order, and a later `group` + `option` pair overwrites an earlier one. An
//! entire `[env:*]` section can therefore live in a file other than
//! `platformio.ini`, and a key such as `framework` or `build_flags` can be
//! replaced from there.
//!
//! fbuild honours that contract instead of reading a subset of the project
//! (FastLED/fbuild#1929). Everything that PlatformIO would silently ignore is
//! refused rather than approximated: a missing file, a wildcard that matches
//! nothing, and an extra file that declares `extra_configs` again (PlatformIO
//! merges the key but never re-processes it, so any nesting is lost).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::parser::parse_ini;
use super::values::parse_list_values;

type Sections = HashMap<String, HashMap<String, String>>;

/// Merge every file named by `[platformio] extra_configs` into `sections`.
///
/// Relative paths and wildcards resolve against the directory holding the base
/// `platformio.ini`, not the process working directory. Returns an error when a
/// declared file cannot be honoured, so no caller ever sees a partial config.
pub(super) fn merge_extra_configs(
    ini_path: &Path,
    sections: &mut Sections,
) -> fbuild_core::Result<()> {
    let patterns = declared_patterns(sections);
    if patterns.is_empty() {
        return Ok(());
    }

    let base_dir = match ini_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let base_key = canonical_key(ini_path);

    for pattern in &patterns {
        for file in resolve_pattern(base_dir, pattern)? {
            if canonical_key(&file) == base_key {
                return Err(config_error(format!(
                    "[platformio] extra_configs lists {pattern}, which resolves to the base \
                     platformio.ini itself; that would merge the file into itself"
                )));
            }
            let content = std::fs::read_to_string(&file).map_err(|error| {
                config_error(format!(
                    "[platformio] extra_configs lists {pattern}, which resolves to {}, but it \
                     could not be read: {error}",
                    file.display()
                ))
            })?;
            let extra = parse_ini(&content)?;
            reject_nested(&file, &extra)?;
            merge_into(sections, extra);
        }
    }

    Ok(())
}

/// The non-empty entries of `[platformio] extra_configs`, in declaration order.
///
/// The parser has already removed inline comments; whole-line comments and
/// blanks declare nothing.
fn declared_patterns(sections: &Sections) -> Vec<String> {
    sections
        .get("platformio")
        .and_then(|platformio| platformio.get("extra_configs"))
        .map(|value| {
            parse_list_values(value)
                .into_iter()
                .filter(|entry| !is_comment_line(entry))
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve one declared entry to the files it names.
///
/// A literal path must exist. A wildcard must match at least one regular file:
/// PlatformIO would expand it to nothing and carry on with a smaller config,
/// which is exactly the silent divergence this module exists to prevent.
fn resolve_pattern(base_dir: &Path, pattern: &str) -> fbuild_core::Result<Vec<PathBuf>> {
    if !has_wildcard(pattern) {
        let path = base_dir.join(pattern);
        if !path.is_file() {
            return Err(config_error(format!(
                "[platformio] extra_configs lists {pattern}, which does not exist \
                 (looked for {}); PlatformIO would reject this project too",
                path.display()
            )));
        }
        return Ok(vec![path]);
    }

    let full = base_dir.join(pattern);
    let Some(pattern_text) = full.to_str() else {
        return Err(config_error(format!(
            "[platformio] extra_configs wildcard {pattern} is not valid UTF-8 once resolved \
             against {}",
            base_dir.display()
        )));
    };
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    };
    let entries = glob::glob_with(pattern_text, options).map_err(|error| {
        config_error(format!(
            "[platformio] extra_configs wildcard {pattern} is not a valid pattern: {error}"
        ))
    })?;

    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let path = entry.map_err(|error| {
            config_error(format!(
                "[platformio] extra_configs wildcard {pattern} could not be expanded: {error}"
            ))
        })?;
        if path.is_file() {
            files.push(path);
        }
    }
    if files.is_empty() {
        return Err(config_error(format!(
            "[platformio] extra_configs wildcard {pattern} matched no files under {}",
            base_dir.display()
        )));
    }
    // PlatformIO leaves wildcard order to the filesystem; sorting makes the
    // "later file wins" rule deterministic instead of directory-order luck.
    files.sort();
    Ok(files)
}

/// Refuse an extra file that tries to pull in further files.
///
/// PlatformIO merges the `extra_configs` option itself but only reads the
/// option from the project config, so the nested include would vanish. Refusing
/// is the only outcome that cannot build from a subset.
fn reject_nested(file: &Path, extra: &Sections) -> fbuild_core::Result<()> {
    let nested = declared_patterns(extra);
    if nested.is_empty() {
        return Ok(());
    }
    Err(config_error(format!(
        "{} declares [platformio] extra_configs = {}, but PlatformIO does not process \
         extra_configs recursively, so those files would never be read. Inline the settings, or \
         list every file from the base platformio.ini",
        file.display(),
        nested.join(", ")
    )))
}

/// Overwrite the base sections with the extra file's values (later wins).
fn merge_into(sections: &mut Sections, extra: Sections) {
    for (name, values) in extra {
        let target = sections.entry(name).or_default();
        for (key, value) in values {
            target.insert(key, value);
        }
    }
}

/// A stable comparison key even when the path does not exist.
fn canonical_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn has_wildcard(pattern: &str) -> bool {
    pattern.contains(['*', '?', '['])
}

fn is_comment_line(line: &str) -> bool {
    line.starts_with(';') || line.starts_with('#')
}

fn config_error(message: String) -> fbuild_core::FbuildError {
    fbuild_core::FbuildError::ConfigError(message)
}
