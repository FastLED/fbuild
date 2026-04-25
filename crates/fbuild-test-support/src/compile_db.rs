//! Parser for clangd-style `compile_commands.json` files.
//!
//! See <https://clang.llvm.org/docs/JSONCompilationDatabase.html> for the
//! format spec. Each entry is an object with required `directory` and `file`
//! fields and one of `command` (a single shell-quoted string) or `arguments`
//! (a JSON array of pre-tokenized argv). An optional `output` field names the
//! emitted artifact.
//!
//! This module is intentionally small: it powers test-suite assertions over
//! the contents of `.fbuild/compile_commands.json` (TU counts, presence of
//! files under specific subtrees, etc.) without forcing every test to
//! reimplement JSON walking.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// One row from a clangd `compile_commands.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileEntry {
    /// Working directory for the command (absolute, as written in JSON).
    pub directory: PathBuf,
    /// Source file path. If the JSON value was relative it is joined onto
    /// `directory` (without canonicalization, since the file may not exist
    /// on test runners).
    pub file: PathBuf,
    /// Output object path, resolved like `file`. `None` when the JSON entry
    /// omits the field.
    pub output: Option<PathBuf>,
    /// Either the parsed `arguments` array verbatim, or `command` split via
    /// shell-style tokenization (`shell-words` crate semantics).
    pub arguments: Vec<String>,
}

/// Parsed `compile_commands.json`.
#[derive(Debug, Clone, Default)]
pub struct CompileDb {
    entries: Vec<CompileEntry>,
}

/// Errors produced by [`CompileDb`] parsing.
#[derive(Debug, thiserror::Error)]
pub enum CompileDbError {
    /// I/O failure while reading the file from disk.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The bytes were not valid JSON.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// The JSON parsed but did not match the compilation-database schema.
    #[error("malformed compile_commands.json: {0}")]
    Malformed(String),
}

impl CompileDb {
    /// Read and parse a compilation database from disk.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Result<Self, CompileDbError> {
        let text = std::fs::read_to_string(path.as_ref())?;
        Self::from_str(&text)
    }

    /// Parse a compilation database from an in-memory JSON string.
    ///
    /// When an entry has both `arguments` and `command`, `arguments` wins
    /// (matching clangd behavior). `command` is tokenized with the
    /// `shell-words` crate, which handles single quotes, double quotes, and
    /// backslash escapes the same way POSIX `sh` does.
    #[allow(clippy::should_implement_trait)] // intentional: returns CompileDbError, not FromStr::Err
    pub fn from_str(json: &str) -> Result<Self, CompileDbError> {
        let value: serde_json::Value = serde_json::from_str(json)?;
        let array = value
            .as_array()
            .ok_or_else(|| CompileDbError::Malformed("top-level value is not an array".into()))?;

        let mut entries = Vec::with_capacity(array.len());
        for (idx, item) in array.iter().enumerate() {
            let obj = item.as_object().ok_or_else(|| {
                CompileDbError::Malformed(format!("entry {idx} is not an object"))
            })?;

            let directory = obj
                .get("directory")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    CompileDbError::Malformed(format!(
                        "entry {idx} missing required string field `directory`"
                    ))
                })?;
            let directory = PathBuf::from(directory);

            let file_raw = obj
                .get("file")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    CompileDbError::Malformed(format!(
                        "entry {idx} missing required string field `file`"
                    ))
                })?;
            let file = resolve(&directory, file_raw);

            let output = match obj.get("output") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => {
                    let s = v.as_str().ok_or_else(|| {
                        CompileDbError::Malformed(format!(
                            "entry {idx} field `output` is not a string"
                        ))
                    })?;
                    Some(resolve(&directory, s))
                }
            };

            let arguments = if let Some(arr) = obj.get("arguments") {
                let arr = arr.as_array().ok_or_else(|| {
                    CompileDbError::Malformed(format!(
                        "entry {idx} field `arguments` is not an array"
                    ))
                })?;
                let mut out = Vec::with_capacity(arr.len());
                for (ai, av) in arr.iter().enumerate() {
                    let s = av.as_str().ok_or_else(|| {
                        CompileDbError::Malformed(format!(
                            "entry {idx} arguments[{ai}] is not a string"
                        ))
                    })?;
                    out.push(s.to_owned());
                }
                out
            } else if let Some(cmd) = obj.get("command") {
                let cmd = cmd.as_str().ok_or_else(|| {
                    CompileDbError::Malformed(format!(
                        "entry {idx} field `command` is not a string"
                    ))
                })?;
                shell_words::split(cmd).map_err(|e| {
                    CompileDbError::Malformed(format!(
                        "entry {idx} `command` shell-split failed: {e}"
                    ))
                })?
            } else {
                return Err(CompileDbError::Malformed(format!(
                    "entry {idx} requires one of `arguments` or `command`"
                )));
            };

            entries.push(CompileEntry {
                directory,
                file,
                output,
                arguments,
            });
        }

        Ok(Self { entries })
    }

    /// All parsed entries in source order.
    pub fn entries(&self) -> &[CompileEntry] {
        &self.entries
    }

    /// Number of raw entries (may exceed [`Self::tu_count`] when a build
    /// system emits duplicate rows for multi-pass compiles).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when there are no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Set of distinct (resolved) source files referenced by any entry.
    pub fn files(&self) -> HashSet<PathBuf> {
        self.entries.iter().map(|e| e.file.clone()).collect()
    }

    /// Translation-unit count: distinct source files.
    pub fn tu_count(&self) -> usize {
        self.files().len()
    }

    /// Iterator over entries whose `file` path string contains `needle`.
    ///
    /// Useful for assertions like "no entries reference `libraries/FNET/`".
    pub fn entries_matching<'a>(
        &'a self,
        needle: &'a str,
    ) -> impl Iterator<Item = &'a CompileEntry> + 'a {
        self.entries
            .iter()
            .filter(move |e| path_contains(&e.file, needle))
    }

    /// Returns the subset of `needles` that match at least one entry's `file`.
    ///
    /// Designed for crisp failure messages: an empty result means "clean";
    /// a non-empty result lists exactly which forbidden subtrees leaked in.
    pub fn forbidden_present(&self, needles: &[&str]) -> Vec<String> {
        needles
            .iter()
            .filter(|needle| self.entries.iter().any(|e| path_contains(&e.file, needle)))
            .map(|s| (*s).to_owned())
            .collect()
    }
}

fn resolve(directory: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        directory.join(p)
    }
}

fn path_contains(path: &Path, needle: &str) -> bool {
    // Match against the path's string form. Use `to_string_lossy` so that
    // non-UTF-8 paths still produce a best-effort haystack rather than
    // silently skipping the entry.
    path.to_string_lossy().contains(needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_parses_arguments_form() {
        let json = r#"[
            {
                "directory": "/work",
                "file": "src/main.cpp",
                "arguments": ["clang", "-c", "src/main.cpp"]
            }
        ]"#;
        let db = CompileDb::from_str(json).unwrap();
        assert_eq!(db.len(), 1);
        let e = &db.entries()[0];
        assert_eq!(e.directory, PathBuf::from("/work"));
        assert_eq!(e.file, PathBuf::from("/work").join("src/main.cpp"));
        assert_eq!(e.arguments, vec!["clang", "-c", "src/main.cpp"]);
        assert_eq!(e.output, None);
    }

    #[test]
    fn from_str_parses_command_form_with_shell_split() {
        let json = r#"[
            {
                "directory": "/work",
                "file": "src/path with spaces.cpp",
                "command": "clang -c -DFOO=1 \"src/path with spaces.cpp\""
            }
        ]"#;
        let db = CompileDb::from_str(json).unwrap();
        let args = &db.entries()[0].arguments;
        assert_eq!(
            args,
            &vec![
                "clang".to_string(),
                "-c".to_string(),
                "-DFOO=1".to_string(),
                "src/path with spaces.cpp".to_string(),
            ]
        );
    }

    #[test]
    fn from_str_arguments_takes_priority_over_command() {
        let json = r#"[
            {
                "directory": "/w",
                "file": "a.c",
                "arguments": ["from-args"],
                "command": "from-command should-be-ignored"
            }
        ]"#;
        let db = CompileDb::from_str(json).unwrap();
        assert_eq!(db.entries()[0].arguments, vec!["from-args".to_string()]);
    }

    #[test]
    fn from_path_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compile_commands.json");
        std::fs::write(
            &path,
            r#"[{"directory":"/w","file":"a.c","arguments":["cc","a.c"]}]"#,
        )
        .unwrap();
        let db = CompileDb::from_path(&path).unwrap();
        assert_eq!(db.len(), 1);
    }

    #[test]
    fn relative_paths_resolved_against_directory() {
        let json = r#"[
            {
                "directory": "/work",
                "file": "src/main.cpp",
                "output": "build/main.o",
                "arguments": ["cc"]
            }
        ]"#;
        let db = CompileDb::from_str(json).unwrap();
        let e = &db.entries()[0];
        assert_eq!(e.file, PathBuf::from("/work").join("src/main.cpp"));
        assert_eq!(e.output, Some(PathBuf::from("/work").join("build/main.o")));
    }

    #[test]
    fn output_is_optional() {
        let json = r#"[
            {"directory":"/w","file":"a.c","arguments":["cc","a.c"]}
        ]"#;
        let db = CompileDb::from_str(json).unwrap();
        assert_eq!(db.entries()[0].output, None);
    }

    #[test]
    fn tu_count_dedupes_repeated_files() {
        let json = r#"[
            {"directory":"/w","file":"a.c","arguments":["cc","-O0","a.c"]},
            {"directory":"/w","file":"a.c","arguments":["cc","-O2","a.c"]}
        ]"#;
        let db = CompileDb::from_str(json).unwrap();
        assert_eq!(db.len(), 2);
        assert_eq!(db.tu_count(), 1);
    }

    #[test]
    fn entries_matching_filters_by_substring() {
        let json = r#"[
            {"directory":"/w","file":"libraries/SPI/SPI.cpp","arguments":["cc"]},
            {"directory":"/w","file":"libraries/FNET/fnet.c","arguments":["cc"]},
            {"directory":"/w","file":"src/main.cpp","arguments":["cc"]}
        ]"#;
        let db = CompileDb::from_str(json).unwrap();
        let hits: Vec<_> = db.entries_matching("FNET").collect();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].file.to_string_lossy().contains("FNET"));
    }

    #[test]
    fn forbidden_present_returns_only_hit_needles() {
        let json = r#"[
            {"directory":"/w","file":"libraries/FNET/fnet.c","arguments":["cc"]}
        ]"#;
        let db = CompileDb::from_str(json).unwrap();
        let hits = db.forbidden_present(&["FNET", "Snooze"]);
        assert_eq!(hits, vec!["FNET".to_string()]);
    }

    #[test]
    fn malformed_json_returns_json_error() {
        let err = CompileDb::from_str("not json").unwrap_err();
        assert!(matches!(err, CompileDbError::Json(_)));
    }

    #[test]
    fn entry_without_file_field_returns_malformed() {
        let json = r#"[{"directory":"/w","arguments":["cc"]}]"#;
        let err = CompileDb::from_str(json).unwrap_err();
        assert!(matches!(err, CompileDbError::Malformed(_)));
    }
}
