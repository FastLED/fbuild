//! Framework-library resolution shared across platform orchestrators.
//!
//! PlatformIO ships Arduino-style frameworks (Teensyduino, STM32duino, ...)
//! with a `libraries/` directory containing bundled libraries like `SPI` and
//! `Wire`. A sketch that does `#include <SPI.h>` must get the library's
//! include dirs on the compiler's search path and its sources linked in.
//!
//! This module walks project sources for `#include` directives, matches them
//! against the library's exported headers, and returns the set of source
//! files that must be compiled. It transitively follows includes inside
//! selected libraries so `A.h -> B.h` pulls in B as well, and it shadows
//! framework libraries with project-local copies of the same name so a user
//! can override a bundled library by vendoring it under `lib/<name>/`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use fbuild_packages::library::FrameworkLibrary;
use walkdir::{DirEntry, WalkDir};

/// Resolve framework library source files needed by a project.
pub fn resolve_framework_library_sources(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
) -> Vec<PathBuf> {
    let roots = framework_include_scan_roots(project_dir, src_dir);
    resolve_framework_library_sources_from_libraries(libraries, &roots)
}

/// Selection algorithm: build a header-to-library map, transitively follow
/// includes from project sources, prefer project-local headers, emit the
/// selected libraries' source files deduped and sorted.
pub fn resolve_framework_library_sources_from_libraries(
    libraries: &[FrameworkLibrary],
    roots: &[PathBuf],
) -> Vec<PathBuf> {
    let mut header_to_library = HashMap::new();
    for (idx, library) in libraries.iter().enumerate() {
        let mut headers = HashSet::new();
        for include_dir in &library.include_dirs {
            collect_header_names(include_dir, &mut headers);
        }
        for header in headers {
            header_to_library.entry(header).or_insert(idx);
        }
    }

    let mut local_headers = HashSet::new();
    for root in roots {
        collect_header_names(root, &mut local_headers);
    }

    let mut pending = HashSet::new();
    for root in roots {
        collect_included_headers(root, &mut pending);
    }

    let mut selected = HashSet::new();
    let mut queue: Vec<String> = pending.iter().cloned().collect();
    while let Some(header) = queue.pop() {
        if local_headers.contains(&header) {
            continue;
        }
        let Some(&library_idx) = header_to_library.get(&header) else {
            continue;
        };
        if !selected.insert(library_idx) {
            continue;
        }

        let mut transitive_headers = HashSet::new();
        collect_framework_included_headers(&libraries[library_idx].dir, &mut transitive_headers);
        for transitive in transitive_headers {
            if pending.insert(transitive.clone()) {
                queue.push(transitive);
            }
        }
    }

    let mut selected_indices: Vec<_> = selected.into_iter().collect();
    selected_indices.sort_unstable();

    let mut sources = Vec::new();
    for idx in selected_indices {
        tracing::info!(
            "selected framework library '{}': {} source files",
            libraries[idx].name,
            libraries[idx].source_files.len()
        );
        sources.extend(libraries[idx].source_files.iter().cloned());
    }
    sources.sort();
    sources.dedup();
    sources
}

/// Project directories to scan for `#include` directives and local headers.
pub fn framework_include_scan_roots(project_dir: &Path, src_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    push_existing_unique(&mut roots, src_dir.to_path_buf());
    push_existing_unique(&mut roots, project_dir.join("src"));
    push_existing_unique(&mut roots, project_dir.join("include"));
    push_existing_unique(&mut roots, project_dir.join("lib"));
    roots
}

fn push_existing_unique(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if !path.exists() {
        return;
    }
    if !roots.iter().any(|existing| existing == &path) {
        roots.push(path);
    }
}

fn collect_header_names(root: &Path, headers: &mut HashSet<String>) {
    if !root.exists() {
        return;
    }

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(should_scan_framework_entry)
        .flatten()
    {
        if !entry.file_type().is_file() || !is_header_file(entry.path()) {
            continue;
        }
        if let Some(name) = entry.path().file_name().and_then(|name| name.to_str()) {
            headers.insert(name.to_string());
        }
    }
}

fn collect_included_headers(root: &Path, headers: &mut HashSet<String>) {
    collect_included_headers_with_filter(root, headers, should_scan_entry);
}

fn collect_framework_included_headers(root: &Path, headers: &mut HashSet<String>) {
    collect_included_headers_with_filter(root, headers, should_scan_framework_entry);
}

fn collect_included_headers_with_filter(
    root: &Path,
    headers: &mut HashSet<String>,
    filter: fn(&DirEntry) -> bool,
) {
    if !root.exists() {
        return;
    }

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(filter)
        .flatten()
    {
        if !entry.file_type().is_file() || !is_source_or_header_file(entry.path()) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        for line in content.lines() {
            if let Some(header) = parse_include_header(line) {
                headers.insert(header);
            }
        }
    }
}

fn should_scan_entry(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy().to_lowercase();
    !matches!(
        name.as_str(),
        ".git"
            | ".pio"
            | ".fbuild"
            | ".zap"
            | ".build"
            | "build"
            | "target"
            | ".venv"
            | "venv"
            | "node_modules"
            | "__pycache__"
    )
}

fn should_scan_framework_entry(entry: &DirEntry) -> bool {
    if !should_scan_entry(entry) {
        return false;
    }
    let name = entry.file_name().to_string_lossy().to_lowercase();
    !matches!(
        name.as_str(),
        "examples" | "example" | "extras" | "test" | "tests" | "fontconvert"
    )
}

fn is_source_or_header_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_lowercase();
    matches!(
        ext.as_str(),
        "c" | "cpp" | "cc" | "cxx" | "s" | "ino" | "h" | "hh" | "hpp" | "hxx"
    )
}

fn is_header_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_lowercase();
    matches!(ext.as_str(), "h" | "hh" | "hpp" | "hxx")
}

fn parse_include_header(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let directive = trimmed.strip_prefix('#')?.trim_start();
    let rest = directive.strip_prefix("include")?.trim_start();
    let mut chars = rest.chars();
    let opener = chars.next()?;
    let closer = match opener {
        '<' => '>',
        '"' => '"',
        _ => return None,
    };
    let remainder = &rest[opener.len_utf8()..];
    let end = remainder.find(closer)?;
    let include_path = &remainder[..end];
    Path::new(include_path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_include_extracts_basename() {
        assert_eq!(
            parse_include_header("#include <SPI.h>"),
            Some("SPI.h".to_string())
        );
        assert_eq!(
            parse_include_header("  # include \"utility/foo.hpp\""),
            Some("foo.hpp".to_string())
        );
        assert_eq!(parse_include_header("int x = 1;"), None);
    }

    #[test]
    fn resolves_libraries_from_project_includes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(
            project_src.join("main.cpp"),
            "#include <SPI.h>\n#include <OctoWS2811.h>\n",
        )
        .unwrap();

        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let octo_dir = tmp
            .path()
            .join("framework")
            .join("libraries")
            .join("OctoWS2811");
        std::fs::create_dir_all(&octo_dir).unwrap();
        std::fs::write(octo_dir.join("OctoWS2811.h"), "").unwrap();
        std::fs::write(octo_dir.join("OctoWS2811.cpp"), "").unwrap();
        std::fs::write(octo_dir.join("OctoWS2811_imxrt.cpp"), "").unwrap();

        let libraries = vec![
            FrameworkLibrary {
                name: "OctoWS2811".to_string(),
                dir: octo_dir.clone(),
                include_dirs: vec![octo_dir.clone()],
                source_files: vec![
                    octo_dir.join("OctoWS2811.cpp"),
                    octo_dir.join("OctoWS2811_imxrt.cpp"),
                ],
            },
            FrameworkLibrary {
                name: "SPI".to_string(),
                dir: spi_dir.clone(),
                include_dirs: vec![spi_dir.clone()],
                source_files: vec![spi_dir.join("SPI.cpp")],
            },
        ];

        let mut sources = resolve_framework_library_sources_from_libraries(
            &libraries,
            std::slice::from_ref(&project_src),
        );
        sources.sort();

        assert_eq!(
            sources,
            vec![
                octo_dir.join("OctoWS2811.cpp"),
                octo_dir.join("OctoWS2811_imxrt.cpp"),
                spi_dir.join("SPI.cpp"),
            ]
        );
    }

    #[test]
    fn follows_transitive_includes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(project_src.join("main.cpp"), "#include <NeedsSpi.h>\n").unwrap();

        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let wrapper_dir = tmp
            .path()
            .join("framework")
            .join("libraries")
            .join("NeedsSpi");
        std::fs::create_dir_all(&wrapper_dir).unwrap();
        std::fs::write(wrapper_dir.join("NeedsSpi.h"), "#include <SPI.h>\n").unwrap();
        std::fs::write(wrapper_dir.join("NeedsSpi.cpp"), "").unwrap();

        let libraries = vec![
            FrameworkLibrary {
                name: "NeedsSpi".to_string(),
                dir: wrapper_dir.clone(),
                include_dirs: vec![wrapper_dir.clone()],
                source_files: vec![wrapper_dir.join("NeedsSpi.cpp")],
            },
            FrameworkLibrary {
                name: "SPI".to_string(),
                dir: spi_dir.clone(),
                include_dirs: vec![spi_dir.clone()],
                source_files: vec![spi_dir.join("SPI.cpp")],
            },
        ];

        let mut sources = resolve_framework_library_sources_from_libraries(
            &libraries,
            std::slice::from_ref(&project_src),
        );
        sources.sort();

        assert_eq!(
            sources,
            vec![wrapper_dir.join("NeedsSpi.cpp"), spi_dir.join("SPI.cpp")]
        );
    }

    #[test]
    fn prefers_local_library_over_framework() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        let project_lib = tmp
            .path()
            .join("project")
            .join("lib")
            .join("FastLED")
            .join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::create_dir_all(&project_lib).unwrap();
        std::fs::write(project_src.join("main.cpp"), "#include <FastLED.h>\n").unwrap();
        std::fs::write(project_lib.join("FastLED.h"), "#include <SPI.h>\n").unwrap();
        std::fs::write(project_lib.join("FastLED.cpp"), "").unwrap();

        let framework_fastled_dir = tmp
            .path()
            .join("framework")
            .join("libraries")
            .join("FastLED");
        std::fs::create_dir_all(&framework_fastled_dir).unwrap();
        std::fs::write(framework_fastled_dir.join("FastLED.h"), "").unwrap();
        std::fs::write(framework_fastled_dir.join("FastLED.cpp"), "").unwrap();

        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let libraries = vec![
            FrameworkLibrary {
                name: "FastLED".to_string(),
                dir: framework_fastled_dir.clone(),
                include_dirs: vec![framework_fastled_dir.clone()],
                source_files: vec![framework_fastled_dir.join("FastLED.cpp")],
            },
            FrameworkLibrary {
                name: "SPI".to_string(),
                dir: spi_dir.clone(),
                include_dirs: vec![spi_dir.clone()],
                source_files: vec![spi_dir.join("SPI.cpp")],
            },
        ];

        let roots = vec![project_src, project_lib];
        let sources = resolve_framework_library_sources_from_libraries(&libraries, &roots);

        assert_eq!(sources, vec![spi_dir.join("SPI.cpp")]);
    }
}
