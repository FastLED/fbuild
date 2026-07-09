//! Filesystem helpers for the ESP32 framework module.

use std::path::{Path, PathBuf};

/// Find the actual framework root inside an extracted archive.
/// Recursively copy a directory tree.
pub(crate) fn copy_dir_recursive(src: &Path, dest: &Path) -> fbuild_core::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else {
            std::fs::copy(&src_path, &dest_path)?;
        }
    }
    Ok(())
}

pub(crate) fn find_framework_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("cores").exists() {
        return install_dir.to_path_buf();
    }

    // Check one level deep
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("cores").exists() {
                return path;
            }
        }
    }

    install_dir.to_path_buf()
}

/// Recursively scan for include paths that are useful for compilation.
///
/// A directory is added if it contains headers directly OR has an immediate
/// child directory with headers (supporting `#include "subdir/header.h"`).
/// This matches PlatformIO's include strategy without adding every directory.
///
/// Used for the 2.x framework layout where `flags/includes` doesn't exist.
pub(crate) fn scan_include_dirs_recursive(
    dir: &Path,
    dirs: &mut Vec<PathBuf>,
    depth: usize,
    max_depth: usize,
) {
    if depth > max_depth {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut has_headers = false;
        let mut child_has_headers = false;
        let mut subdirs = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Check if this child dir has headers (one level peek)
                if !child_has_headers && dir_contains_headers(&path) {
                    child_has_headers = true;
                }
                subdirs.push(path);
            } else if !has_headers {
                if let Some(ext) = path.extension() {
                    if ext == "h" || ext == "hpp" {
                        has_headers = true;
                    }
                }
            }
        }

        // Add if dir has headers or a child dir has headers
        if has_headers || child_has_headers {
            dirs.push(dir.to_path_buf());
        }

        for subdir in subdirs {
            scan_include_dirs_recursive(&subdir, dirs, depth + 1, max_depth);
        }
    }
}

/// Check if a directory directly contains any header files.
fn dir_contains_headers(dir: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "h" || ext == "hpp" {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Collect all `.a` archive files from a directory (non-recursive).
pub(crate) fn collect_archive_files(dir: &Path) -> Vec<PathBuf> {
    let mut libs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "a") {
                libs.push(path);
            }
        }
    }
    libs.sort();
    libs
}

/// Collect source files from a directory (non-recursive).
pub(crate) fn collect_sources(dir: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let ext = path
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                if matches!(ext.as_str(), "c" | "cpp" | "cc" | "s") {
                    sources.push(path);
                }
            }
        }
    }
    sources.sort();
    sources
}
