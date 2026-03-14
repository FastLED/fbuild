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
}

impl InstalledLibrary {
    pub fn new(lib_dir: &Path, name: &str) -> Self {
        Self {
            lib_dir: lib_dir.to_path_buf(),
            name: name.to_string(),
        }
    }

    /// Get the source root directory.
    ///
    /// Handles the `src/src/` pattern (e.g., FastLED): if `src/src/` exists,
    /// use that; otherwise use `src/`.
    pub fn source_root(&self) -> PathBuf {
        let src = self.lib_dir.join("src");
        let src_src = src.join("src");
        if src_src.exists() && src_src.is_dir() {
            src_src
        } else {
            src
        }
    }

    /// Get include directories for this library.
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

    /// Get all source files (.c, .cpp, .cc, .cxx) in the library.
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

    /// Check if the library is header-only (no source files to compile).
    pub fn is_header_only(&self) -> bool {
        self.get_source_files().is_empty()
    }

    /// Get the archive output path for this library.
    pub fn archive_path(&self) -> PathBuf {
        self.lib_dir.join(format!("lib{}.a", self.name))
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

/// Check if a file is a C/C++ source file.
fn is_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("c") | Some("cpp") | Some("cc") | Some("cxx")
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
        assert!(lib
            .archive_path()
            .to_string_lossy()
            .contains("libfastled.a"));
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
