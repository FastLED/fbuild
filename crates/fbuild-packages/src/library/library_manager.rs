//! Top-level library dependency orchestrator.
//!
//! Coordinates: spec parsing → download → include discovery → compile → archive.

use std::path::{Path, PathBuf};

use fbuild_core::Result;

use super::library_compiler;
use super::library_downloader;
use super::library_info::InstalledLibrary;
use super::library_spec::LibrarySpec;

/// Result of library resolution and compilation.
pub struct LibraryResult {
    /// All include directories from all libraries (for compiler `-I` flags).
    pub include_dirs: Vec<PathBuf>,
    /// All compiled library archives (`.a` files) for the linker.
    pub archives: Vec<PathBuf>,
}

/// Ensure all library dependencies are downloaded and compiled.
///
/// Flow:
/// 1. Parse specs from `lib_deps`
/// 2. Filter out `lib_ignore` entries
/// 3. Download all libraries
/// 4. Collect all include dirs (needed before compilation for cross-includes)
/// 5. Compile each library
/// 6. Return include dirs + archives
#[allow(clippy::too_many_arguments)]
pub async fn ensure_libraries(
    lib_specs: &[String],
    lib_ignore: &[String],
    gcc_path: &Path,
    gxx_path: &Path,
    ar_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    base_includes: &[PathBuf],
    libs_dir: &Path,
    verbose: bool,
) -> Result<LibraryResult> {
    // 1. Parse specs, filter ignored
    let specs: Vec<LibrarySpec> = lib_specs
        .iter()
        .filter_map(|s| LibrarySpec::parse(s))
        .filter(|spec| {
            !lib_ignore
                .iter()
                .any(|ig| ig.eq_ignore_ascii_case(&spec.name))
        })
        .collect();

    if specs.is_empty() {
        return Ok(LibraryResult {
            include_dirs: Vec::new(),
            archives: Vec::new(),
        });
    }

    tracing::info!("resolving {} library dependencies", specs.len());

    // 2. Download all libraries
    std::fs::create_dir_all(libs_dir)?;
    let mut installed: Vec<InstalledLibrary> = Vec::new();

    for spec in &specs {
        let lib_dir = library_downloader::download_library(spec, libs_dir).await?;
        installed.push(InstalledLibrary::new(&lib_dir, &spec.sanitized_name()));
    }

    // 3. Collect all include dirs (needed for cross-library includes)
    let mut all_include_dirs: Vec<PathBuf> = base_includes.to_vec();
    for lib in &installed {
        all_include_dirs.extend(lib.get_include_dirs());
    }

    // 4. Compile each library
    let mut archives = Vec::new();
    for lib in &installed {
        if lib.is_header_only() {
            tracing::info!("library {} is header-only", lib.name);
            continue;
        }

        // Check if archive already exists
        let archive = lib.archive_path();
        if archive.exists() {
            tracing::debug!("library {} already compiled", lib.name);
            archives.push(archive);
            continue;
        }

        let sources = lib.get_source_files();
        if let Some(archive_path) = library_compiler::compile_library(
            &lib.name,
            &sources,
            &all_include_dirs,
            gcc_path,
            gxx_path,
            ar_path,
            c_flags,
            cpp_flags,
            &lib.lib_dir,
            verbose,
        )? {
            archives.push(archive_path);
        }
    }

    // Return include dirs (library includes only, not base includes)
    let lib_include_dirs: Vec<PathBuf> = installed
        .iter()
        .flat_map(|lib| lib.get_include_dirs())
        .collect();

    Ok(LibraryResult {
        include_dirs: lib_include_dirs,
        archives,
    })
}

/// Synchronous wrapper for ensure_libraries.
#[allow(clippy::too_many_arguments)]
pub fn ensure_libraries_sync(
    lib_specs: &[String],
    lib_ignore: &[String],
    gcc_path: &Path,
    gxx_path: &Path,
    ar_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    base_includes: &[PathBuf],
    libs_dir: &Path,
    verbose: bool,
) -> Result<LibraryResult> {
    let rt = tokio::runtime::Handle::try_current().ok();
    if let Some(handle) = rt {
        handle.block_on(ensure_libraries(
            lib_specs,
            lib_ignore,
            gcc_path,
            gxx_path,
            ar_path,
            c_flags,
            cpp_flags,
            base_includes,
            libs_dir,
            verbose,
        ))
    } else {
        let rt = tokio::runtime::Runtime::new().map_err(|e| {
            fbuild_core::FbuildError::PackageError(format!("failed to create tokio runtime: {}", e))
        })?;
        rt.block_on(ensure_libraries(
            lib_specs,
            lib_ignore,
            gcc_path,
            gxx_path,
            ar_path,
            c_flags,
            cpp_flags,
            base_includes,
            libs_dir,
            verbose,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_specs() {
        let result = ensure_libraries_sync(
            &[],
            &[],
            Path::new("/gcc"),
            Path::new("/g++"),
            Path::new("/ar"),
            &[],
            &[],
            &[],
            Path::new("/libs"),
            false,
        )
        .unwrap();
        assert!(result.include_dirs.is_empty());
        assert!(result.archives.is_empty());
    }

    #[test]
    fn test_all_ignored() {
        let result = ensure_libraries_sync(
            &["FastLED".to_string()],
            &["FastLED".to_string()],
            Path::new("/gcc"),
            Path::new("/g++"),
            Path::new("/ar"),
            &[],
            &[],
            &[],
            Path::new("/libs"),
            false,
        )
        .unwrap();
        assert!(result.include_dirs.is_empty());
        assert!(result.archives.is_empty());
    }

    #[test]
    fn test_local_path_specs_skipped() {
        let result = ensure_libraries_sync(
            &["symlink://./local".to_string()],
            &[],
            Path::new("/gcc"),
            Path::new("/g++"),
            Path::new("/ar"),
            &[],
            &[],
            &[],
            Path::new("/libs"),
            false,
        )
        .unwrap();
        assert!(result.include_dirs.is_empty());
    }
}
