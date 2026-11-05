//! Library include and source file discovery.
//!
//! Scans downloaded libraries to find include directories and source files,
//! handling common Arduino library layouts (src/, src/src/, include/).

use std::path::{Path, PathBuf};

/// An installed library with discovered include dirs and source files.
pub struct InstalledLibrary {
    /// Library root directory (contains src/, library.json, etc.)
    pub lib_dir: PathBuf,
    /// Library name.
    pub name: String,
    /// Directory for generated objects and archives.  Local dependencies keep
    /// this separate from their source checkout.
    pub build_dir: PathBuf,
}

impl InstalledLibrary {
    pub fn new(lib_dir: &Path, name: &str) -> Self {
        Self {
            lib_dir: lib_dir.to_path_buf(),
            name: name.to_string(),
            build_dir: lib_dir.to_path_buf(),
        }
    }

    /// Construct a library whose generated artifacts belong in `build_dir`.
    pub fn with_build_dir(lib_dir: &Path, name: &str, build_dir: &Path) -> Self {
        Self {
            lib_dir: lib_dir.to_path_buf(),
            name: name.to_string(),
            build_dir: build_dir.to_path_buf(),
        }
    }

    /// Get the source root directory.
    ///
    /// Handles the `src/src/` pattern (e.g., FastLED): if `src/src/` exists,
    /// use that; if `src/` exists, use that; otherwise fall back to the
    /// library root (PlatformIO scans the whole library when no `src/` dir).
    pub fn source_root(&self) -> PathBuf {
        let src = self.lib_dir.join("src");
        let src_src = src.join("src");
        if src_src.exists() && src_src.is_dir() {
            src_src
        } else if src.exists() && src.is_dir() {
            src
        } else {
            self.lib_dir.clone()
        }
    }

    /// Unfiltered diagnostic view of conventional include directories.
    /// Production builders must use the fallible manifest-aware variant.
    ///
    /// Returns directories that should be added to `-I` flags.
    pub fn get_include_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        let src = self.lib_dir.join("src");
        if !src.exists() {
            return dirs;
        }

        // Check for src/src/ structure (FastLED pattern)
        let src_src = src.join("src");
        if src_src.exists() && src_src.is_dir() {
            dirs.push(src_src);
        } else {
            dirs.push(src);
        }

        // Look for additional include directories
        let inc_dir = self.lib_dir.join("include");
        if inc_dir.exists() && !dirs.contains(&inc_dir) {
            dirs.push(inc_dir);
        }

        dirs
    }

    /// Unfiltered diagnostic view of source files (.c, .cpp, .cc, .cxx).
    /// Production builders must use the fallible manifest-aware variant.
    ///
    /// Skips `example*/` and `test*/` directories.
    pub fn get_source_files(&self) -> Vec<PathBuf> {
        let search_dir = self.source_root();
        if !search_dir.exists() {
            return Vec::new();
        }

        let mut sources = Vec::new();
        collect_sources(&search_dir, &mut sources);
        sources.sort();
        sources
    }

    /// Apply a library manifest's ordered source filter relative to its srcDir.
    /// Fallible so malformed metadata cannot silently compile excluded files.
    fn manifest_source_filter(
        &self,
    ) -> fbuild_core::Result<(
        fbuild_core::path::NormalizedPath,
        fbuild_config::source_filter::SourceFilter,
    )> {
        let manifest = self.lib_dir.join("library.json");
        let data: serde_json::Value = match std::fs::read_to_string(&manifest) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| {
                fbuild_core::FbuildError::ConfigError(format!("{}: {error}", manifest.display()))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(error) => return Err(error.into()),
        };
        if !data.is_object() {
            return Err(fbuild_core::FbuildError::ConfigError(
                "library manifest must be an object".into(),
            ));
        }
        let build = data.get("build");
        if build.is_some_and(|value| !value.is_object()) {
            return Err(fbuild_core::FbuildError::ConfigError(
                "library build must be an object".into(),
            ));
        }
        let root = match build.and_then(|value| value.get("srcDir")) {
            None => fbuild_core::path::NormalizedPath::new(self.source_root()),
            Some(serde_json::Value::String(path)) if !path.is_empty() => {
                let directory = fbuild_core::path::NormalizedPath::new(&self.lib_dir).join(path);
                if !directory.is_dir() {
                    return Err(fbuild_core::FbuildError::ConfigError(format!(
                        "library build.srcDir must name an existing directory: {directory}"
                    )));
                }
                directory
            }
            Some(_) => {
                return Err(fbuild_core::FbuildError::ConfigError(
                    "library build.srcDir must be a nonempty string".into(),
                ));
            }
        };
        let spec = match build.and_then(|value| value.get("srcFilter")) {
            None => None,
            Some(serde_json::Value::String(value)) => Some(value.clone()),
            Some(serde_json::Value::Array(values)) => Some(
                values
                    .iter()
                    .map(|value| {
                        value.as_str().map(str::to_owned).ok_or_else(|| {
                            fbuild_core::FbuildError::ConfigError(
                                "library build.srcFilter array must contain strings".into(),
                            )
                        })
                    })
                    .collect::<fbuild_core::Result<Vec<_>>>()?
                    .join("\n"),
            ),
            Some(_) => {
                return Err(fbuild_core::FbuildError::ConfigError(
                    "library build.srcFilter must be string or array".into(),
                ));
            }
        };
        let filter = fbuild_config::source_filter::SourceFilter::parse(spec.as_deref())?;
        Ok((root, filter))
    }

    /// Fallible include discovery honoring the same manifest srcDir as source selection.
    pub fn get_include_dirs_filtered(&self) -> fbuild_core::Result<Vec<PathBuf>> {
        let (root, _) = self.manifest_source_filter()?;
        let mut dirs = vec![root.into_path_buf()];
        let include = self.lib_dir.join("include");
        if include.is_dir() && !dirs.contains(&include) {
            dirs.push(include);
        }
        Ok(dirs)
    }

    /// Resolve and validate the manifest source root without swallowing metadata errors.
    pub fn source_root_filtered(&self) -> fbuild_core::Result<fbuild_core::path::NormalizedPath> {
        self.manifest_source_filter().map(|(root, _)| root)
    }

    /// Select actual sources with ordered manifest filters; malformed data is an error.
    pub fn get_source_files_filtered(&self) -> fbuild_core::Result<Vec<PathBuf>> {
        let (root, filter) = self.manifest_source_filter()?;
        let mut sources = Vec::new();
        collect_sources(root.as_path(), &mut sources);
        sources.retain(|source| filter.matches(root.as_path(), source));
        sources.sort();
        Ok(sources)
    }

    /// Unfiltered diagnostic header-only check; builders must use filtered sources.
    pub fn is_header_only(&self) -> bool {
        self.get_source_files().is_empty()
    }

    /// Get the archive output path for this library.
    pub fn archive_path(&self) -> PathBuf {
        self.build_dir.join(format!("lib{}.a", self.name))
    }

    /// Environment policy takes precedence over `build.libArchive`; absent
    /// both, libraries retain the default archive/extraction semantics.
    pub fn archive_enabled(&self, environment: Option<bool>) -> fbuild_core::Result<bool> {
        if let Some(enabled) = environment {
            return Ok(enabled);
        }
        let manifest = self.lib_dir.join("library.json");
        let content = match std::fs::read_to_string(&manifest) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error.into()),
        };
        let data: serde_json::Value = serde_json::from_str(&content).map_err(|error| {
            fbuild_core::FbuildError::ConfigError(format!("{}: {error}", manifest.display()))
        })?;
        match data.get("build").and_then(|build| build.get("libArchive")) {
            None => Ok(true),
            Some(serde_json::Value::Bool(enabled)) => Ok(*enabled),
            Some(_) => Err(fbuild_core::FbuildError::ConfigError(format!(
                "{}: build.libArchive must be a boolean",
                manifest.display()
            ))),
        }
    }
}

/// Recursively collect source files, skipping example/test directories.
fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip example and test directories
            let name_lower = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if name_lower.contains("example") || name_lower.contains("test") {
                continue;
            }
            collect_sources(&path, out);
        } else if is_source_file(&path) {
            out.push(path);
        }
    }
}

/// Check if a file is a C/C++/assembly source file.
///
/// `.S` (preprocessed) and `.s` (raw) assembly are intentionally included so
/// libraries that ship hand-written ISRs or arch-specific stubs (e.g.
/// FastLED's `gpio_isr_rx/fast_isr.S` on RISC-V — symbol
/// `gpio_fast_edge_isr`) actually get linked. Without this the library
/// archive is missing the assembly symbols and the final link fails with
/// `undefined reference` errors.
fn is_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("c") | Some("cpp") | Some("cc") | Some("cxx") | Some("S") | Some("s")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_include_dirs_basic() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.h"), "").unwrap();

        let lib = InstalledLibrary::new(tmp.path(), "test");
        let dirs = lib.get_include_dirs();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], src);
    }

    #[test]
    fn test_include_dirs_nested_src() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src_src = tmp.path().join("src").join("src");
        std::fs::create_dir_all(&src_src).unwrap();
        std::fs::write(src_src.join("lib.h"), "").unwrap();

        let lib = InstalledLibrary::new(tmp.path(), "test");
        let dirs = lib.get_include_dirs();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], src_src);
    }

    #[test]
    fn test_include_dirs_with_include_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let include = tmp.path().join("include");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&include).unwrap();

        let lib = InstalledLibrary::new(tmp.path(), "test");
        let dirs = lib.get_include_dirs();
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn test_source_files_basic() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.cpp"), "").unwrap();
        std::fs::write(src.join("helper.c"), "").unwrap();
        std::fs::write(src.join("lib.h"), "").unwrap();

        let lib = InstalledLibrary::new(tmp.path(), "test");
        let files = lib.get_source_files();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| is_source_file(f)));
    }

    #[test]
    fn test_source_files_skips_examples() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let examples = src.join("examples");
        std::fs::create_dir_all(&examples).unwrap();
        std::fs::write(src.join("main.cpp"), "").unwrap();
        std::fs::write(examples.join("demo.cpp"), "").unwrap();

        let lib = InstalledLibrary::new(tmp.path(), "test");
        let files = lib.get_source_files();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_source_files_skips_tests() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let tests = src.join("test");
        std::fs::create_dir_all(&tests).unwrap();
        std::fs::write(src.join("main.cpp"), "").unwrap();
        std::fs::write(tests.join("test_main.cpp"), "").unwrap();

        let lib = InstalledLibrary::new(tmp.path(), "test");
        let files = lib.get_source_files();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_header_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.h"), "").unwrap();

        let lib = InstalledLibrary::new(tmp.path(), "test");
        assert!(lib.is_header_only());
    }

    #[test]
    fn test_not_header_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.cpp"), "").unwrap();

        let lib = InstalledLibrary::new(tmp.path(), "test");
        assert!(!lib.is_header_only());
    }

    #[test]
    fn test_archive_path() {
        let lib = InstalledLibrary::new(Path::new("/libs/fastled"), "fastled");
        assert!(
            lib.archive_path()
                .to_string_lossy()
                .contains("libfastled.a")
        );
    }

    #[test]
    fn test_archive_path_uses_external_build_dir() {
        let lib = InstalledLibrary::with_build_dir(
            Path::new("/source/fastled"),
            "fastled",
            Path::new("/build/libs/fastled"),
        );
        assert_eq!(
            lib.archive_path(),
            PathBuf::from("/build/libs/fastled/libfastled.a")
        );
    }

    #[test]
    fn test_no_src_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lib = InstalledLibrary::new(tmp.path(), "test");
        assert!(lib.get_include_dirs().is_empty());
        assert!(lib.get_source_files().is_empty());
        assert!(lib.is_header_only());
    }
}

#[cfg(test)]
mod manifest_filter_tests {
    use super::*;

    #[test]
    fn ordered_string_and_array_filters_are_relative_to_manifest_src_dir() {
        for spec in [
            serde_json::json!("+<*> -<poison.cpp> +<nested/poison.cpp>"),
            serde_json::json!(["+<*>", "-<poison.cpp>", "+<nested/poison.cpp>"]),
        ] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(temp.path().join("code/nested")).unwrap();
            std::fs::create_dir(temp.path().join("src")).unwrap();
            for path in [
                "code/keep.cpp",
                "code/poison.cpp",
                "code/nested/poison.cpp",
                "src/wrong.cpp",
            ] {
                std::fs::write(temp.path().join(path), "").unwrap();
            }
            std::fs::write(
                temp.path().join("library.json"),
                serde_json::to_vec(&serde_json::json!(
                {"name":"Actual","build":{"srcDir":"code","srcFilter":spec}}))
                .unwrap(),
            )
            .unwrap();
            let library = InstalledLibrary::new(temp.path(), "actual");
            assert_eq!(
                library.get_include_dirs_filtered().unwrap(),
                vec![temp.path().join("code")]
            );
            let sources = library.get_source_files_filtered().unwrap();
            assert_eq!(
                sources,
                vec![
                    temp.path().join("code/keep.cpp"),
                    temp.path().join("code/nested/poison.cpp")
                ]
            );
        }
    }

    #[test]
    fn malformed_manifest_or_rules_are_errors() {
        for data in [
            serde_json::json!(null),
            serde_json::json!({"build":7}),
            serde_json::json!({"build":{"srcDir":false}}),
            serde_json::json!({"build":{"srcFilter":["+<*>",7]}}),
            serde_json::json!({"build":{"srcFilter":"+<*> trailing-garbage"}}),
        ] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::write(
                temp.path().join("library.json"),
                serde_json::to_vec(&data).unwrap(),
            )
            .unwrap();
            assert!(
                InstalledLibrary::new(temp.path(), "bad")
                    .get_source_files_filtered()
                    .is_err(),
                "{data}"
            );
        }
    }
}

#[cfg(test)]
mod explicit_src_dir_tests {
    use super::InstalledLibrary;

    #[test]
    fn explicitly_missing_or_non_directory_src_dir_is_a_configuration_error() {
        for directory in ["missing", "regular-file"] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::write(temp.path().join("regular-file"), "not a directory").unwrap();
            std::fs::write(
                temp.path().join("library.json"),
                serde_json::to_vec(
                    &serde_json::json!({"name":"Actual","build":{"srcDir":directory}}),
                )
                .unwrap(),
            )
            .unwrap();
            let library = InstalledLibrary::new(temp.path(), "actual");
            assert!(
                library.get_source_files_filtered().is_err(),
                "explicit {directory} must not masquerade as header-only"
            );
            assert!(library.get_include_dirs_filtered().is_err());
        }
    }

    #[test]
    fn omitted_root_and_existing_explicit_header_only_directories_remain_valid() {
        for directory in [None, Some("code")] {
            let temp = tempfile::tempdir().unwrap();
            let root = if let Some(directory) = directory {
                let root = temp.path().join(directory);
                std::fs::create_dir(&root).unwrap();
                std::fs::write(
                    temp.path().join("library.json"),
                    serde_json::to_vec(
                        &serde_json::json!({"name":"Actual","build":{"srcDir":directory}}),
                    )
                    .unwrap(),
                )
                .unwrap();
                root
            } else {
                temp.path().to_path_buf()
            };
            std::fs::write(root.join("Actual.h"), "inline int actual() { return 1; }\n").unwrap();
            let library = InstalledLibrary::new(temp.path(), "actual");
            assert!(library.get_source_files_filtered().unwrap().is_empty());
            assert!(library.get_include_dirs_filtered().unwrap().contains(&root));
        }
    }
}
