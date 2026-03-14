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
    let mut downloaded_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    for spec in &specs {
        let lib_dir = library_downloader::download_library(spec, libs_dir).await?;
        installed.push(InstalledLibrary::new(&lib_dir, &spec.sanitized_name()));
        downloaded_names.insert(spec.name.to_lowercase());
    }

    // 2b. Resolve transitive dependencies from library.json files
    let ignore_set: std::collections::HashSet<String> =
        lib_ignore.iter().map(|s| s.to_lowercase()).collect();
    resolve_transitive_deps(&mut installed, &mut downloaded_names, &ignore_set, libs_dir).await?;

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

/// Resolve transitive dependencies by scanning library.json files.
///
/// For each installed library, reads its `library.json` (checking both
/// `lib_dir/library.json` and `lib_dir/src/library.json`) for a `dependencies`
/// array. Downloads any new dependencies and adds them to the installed list.
/// Processes recursively until no new dependencies are found.
async fn resolve_transitive_deps(
    installed: &mut Vec<InstalledLibrary>,
    downloaded_names: &mut std::collections::HashSet<String>,
    lib_ignore: &std::collections::HashSet<String>,
    libs_dir: &Path,
) -> Result<()> {
    let mut queue: Vec<PathBuf> = installed.iter().map(|lib| lib.lib_dir.clone()).collect();

    while let Some(lib_dir) = queue.pop() {
        // Check both possible locations for library.json
        let candidates = [
            lib_dir.join("library.json"),
            lib_dir.join("src").join("library.json"),
        ];

        let mut deps: Vec<serde_json::Value> = Vec::new();
        for candidate in &candidates {
            if !candidate.exists() {
                continue;
            }
            let content = match std::fs::read_to_string(candidate) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let data: serde_json::Value = match serde_json::from_str(&content) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if let Some(dep_list) = data.get("dependencies") {
                match dep_list {
                    serde_json::Value::Array(arr) => {
                        deps = arr.clone();
                        break;
                    }
                    serde_json::Value::Object(_) => {
                        deps = vec![dep_list.clone()];
                        break;
                    }
                    _ => {}
                }
            }
        }

        for dep in deps {
            let dep_obj = match dep.as_object() {
                Some(o) => o,
                None => continue,
            };

            // Filter by platform — only download ESP32-compatible deps
            if let Some(platforms) = dep_obj.get("platforms") {
                let dominated = match platforms {
                    serde_json::Value::Array(arr) => {
                        arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>()
                    }
                    serde_json::Value::String(s) => vec![s.as_str()],
                    _ => vec![],
                };
                if !dominated.is_empty()
                    && !dominated.iter().any(|p| *p == "espressif32" || *p == "*")
                {
                    continue;
                }
            }

            let dep_name = match dep_obj.get("name").and_then(|n| n.as_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            if downloaded_names.contains(&dep_name.to_lowercase()) {
                continue;
            }
            if lib_ignore.contains(&dep_name.to_lowercase()) {
                tracing::debug!("skipping ignored transitive dependency: {}", dep_name);
                continue;
            }

            let dep_owner = dep_obj.get("owner").and_then(|o| o.as_str()).unwrap_or("");
            let dep_version = dep_obj.get("version").and_then(|v| v.as_str());

            let mut spec_str = if dep_owner.is_empty() {
                dep_name.clone()
            } else {
                format!("{}/{}", dep_owner, dep_name)
            };
            if let Some(ver) = dep_version {
                spec_str = format!("{} @ {}", spec_str, ver);
            }

            tracing::info!("resolving transitive dependency: {}", spec_str);

            if let Some(spec) = LibrarySpec::parse(&spec_str) {
                match library_downloader::download_library(&spec, libs_dir).await {
                    Ok(dep_dir) => {
                        let lib = InstalledLibrary::new(&dep_dir, &spec.sanitized_name());
                        queue.push(dep_dir);
                        installed.push(lib);
                        downloaded_names.insert(dep_name.to_lowercase());
                    }
                    Err(e) => {
                        tracing::warn!(
                            "could not resolve transitive dependency '{}': {}",
                            spec_str,
                            e
                        );
                    }
                }
            }
        }
    }

    Ok(())
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
