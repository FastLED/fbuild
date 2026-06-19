//! Source file scanning and .ino preprocessing.
//!
//! Finds .cpp, .c, .S, .ino files in project source directories.
//! Preprocesses .ino files into valid .cpp with function prototypes and an
//! Arduino.h include when the active include roots provide that header.

use regex::Regex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Collection of source files found by the scanner.
#[derive(Debug, Default)]
pub struct SourceCollection {
    /// User sketch sources (.cpp, .c, .S — and preprocessed .ino)
    pub sketch_sources: Vec<PathBuf>,
    /// Arduino core sources
    pub core_sources: Vec<PathBuf>,
    /// Board variant sources
    pub variant_sources: Vec<PathBuf>,
    /// All header files (.h, .hpp) for dependency tracking
    pub headers: Vec<PathBuf>,
}

impl SourceCollection {
    /// All source files (sketch + core + variant).
    pub fn all_sources(&self) -> Vec<&PathBuf> {
        self.sketch_sources
            .iter()
            .chain(self.core_sources.iter())
            .chain(self.variant_sources.iter())
            .collect()
    }
}

/// Directories to exclude from scanning.
const EXCLUDE_DIRS: &[&str] = &[
    ".zap",
    ".pio",
    ".build",
    "build",
    ".git",
    "__pycache__",
    "node_modules",
    ".fbuild",
    ".venv",
    "venv",
    ".cache",
    "target",
    ".vscode",
    ".idea",
];

/// Scans project directories for source files and preprocesses .ino files.
pub struct SourceScanner {
    /// Project source directory (usually `<project>/src`)
    src_dir: PathBuf,
    /// Build output directory (for preprocessed .ino → .cpp)
    build_dir: PathBuf,
}

#[derive(Debug)]
struct SourceFilter {
    rules: Vec<SourceFilterRule>,
    has_include_rules: bool,
}

#[derive(Debug)]
struct SourceFilterRule {
    include: bool,
    matcher: Regex,
}

impl SourceScanner {
    pub fn new(src_dir: &Path, build_dir: &Path) -> Self {
        Self {
            src_dir: src_dir.to_path_buf(),
            build_dir: build_dir.to_path_buf(),
        }
    }

    /// Scan the project source directory for sketch files.
    ///
    /// Returns preprocessed .ino files as .cpp, plus existing .cpp/.c/.S files.
    ///
    /// When a `main.cpp` already `#include`s `.ino` files (PlatformIO convention),
    /// the `.ino` files are NOT preprocessed separately to avoid duplicate symbols.
    pub fn scan_sketch_sources(&self) -> fbuild_core::Result<Vec<PathBuf>> {
        self.scan_sketch_sources_filtered(None)
    }

    /// Scan sketch sources applying a PlatformIO-style source filter, when provided.
    pub fn scan_sketch_sources_filtered(
        &self,
        filter_spec: Option<&str>,
    ) -> fbuild_core::Result<Vec<PathBuf>> {
        self.scan_sketch_sources_filtered_with_include_roots(filter_spec, &[])
    }

    /// Scan sketch sources with known include roots for conditional .ino preprocessing.
    pub fn scan_sketch_sources_filtered_with_include_roots(
        &self,
        filter_spec: Option<&str>,
        include_roots: &[&Path],
    ) -> fbuild_core::Result<Vec<PathBuf>> {
        if !self.src_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sources = Vec::new();
        let mut ino_files = Vec::new();
        let mut has_main_cpp = false;
        let filter = SourceFilter::parse(filter_spec)?;

        for entry in walk_sources(&self.src_dir) {
            if !filter.matches(&self.src_dir, &entry) {
                continue;
            }

            let ext = entry
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            match ext.as_str() {
                "ino" => ino_files.push(entry),
                "cpp" | "c" | "s" | "cc" => {
                    if entry.file_name().is_some_and(|n| n == "main.cpp") {
                        has_main_cpp = true;
                    }
                    sources.push(entry);
                }
                _ => {}
            }
        }

        // If main.cpp exists and includes .ino files, skip preprocessing —
        // the .ino content is already compiled via #include in main.cpp.
        if !ino_files.is_empty() && !has_main_cpp {
            ino_files.sort();
            let preprocessed =
                self.preprocess_ino_files(&ino_files, arduino_header_available(include_roots))?;
            sources.insert(0, preprocessed);
        }

        Ok(sources)
    }

    /// Scan an Arduino core directory for source files.
    pub fn scan_core_sources(&self, core_dir: &Path) -> Vec<PathBuf> {
        if !core_dir.exists() {
            return Vec::new();
        }
        walk_sources(core_dir)
            .into_iter()
            .filter(|p| {
                let ext = p
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                matches!(ext.as_str(), "cpp" | "c" | "s" | "cc")
            })
            .collect()
    }

    /// Scan a board variant directory for source files.
    pub fn scan_variant_sources(&self, variant_dir: &Path) -> Vec<PathBuf> {
        if !variant_dir.exists() {
            return Vec::new();
        }
        walk_sources(variant_dir)
            .into_iter()
            .filter(|p| {
                let ext = p
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                matches!(ext.as_str(), "cpp" | "c" | "s" | "cc")
            })
            .collect()
    }

    /// Scan for all header files in a directory.
    pub fn scan_headers(&self, dir: &Path) -> Vec<PathBuf> {
        if !dir.exists() {
            return Vec::new();
        }
        walk_sources(dir)
            .into_iter()
            .filter(|p| {
                let ext = p
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                matches!(ext.as_str(), "h" | "hpp")
            })
            .collect()
    }

    /// Scan everything: sketch, core, variant sources + headers.
    pub fn scan_all(
        &self,
        core_dir: Option<&Path>,
        variant_dir: Option<&Path>,
    ) -> fbuild_core::Result<SourceCollection> {
        self.scan_all_filtered(core_dir, variant_dir, None)
    }

    /// Scan everything, applying a source filter to sketch files only.
    pub fn scan_all_filtered(
        &self,
        core_dir: Option<&Path>,
        variant_dir: Option<&Path>,
        filter_spec: Option<&str>,
    ) -> fbuild_core::Result<SourceCollection> {
        let include_roots: Vec<&Path> = [core_dir, variant_dir].into_iter().flatten().collect();
        let sketch_sources =
            self.scan_sketch_sources_filtered_with_include_roots(filter_spec, &include_roots)?;
        let core_sources = core_dir
            .map(|d| self.scan_core_sources(d))
            .unwrap_or_default();
        let variant_sources = variant_dir
            .map(|d| self.scan_variant_sources(d))
            .unwrap_or_default();

        let mut headers = self.scan_headers(&self.src_dir);
        if let Some(cd) = core_dir {
            headers.extend(self.scan_headers(cd));
        }
        if let Some(vd) = variant_dir {
            headers.extend(self.scan_headers(vd));
        }

        Ok(SourceCollection {
            sketch_sources,
            core_sources,
            variant_sources,
            headers,
        })
    }

    /// Preprocess .ino files into a single .cpp file.
    ///
    /// 1. Concatenate .ino files (alphabetically sorted)
    /// 2. Add `#include <Arduino.h>` at top when available
    /// 3. Extract function prototypes
    /// 4. Add prototypes before first function definition
    /// 5. Add `#line` directives for debugging
    fn preprocess_ino_files(
        &self,
        ino_files: &[PathBuf],
        include_arduino_h: bool,
    ) -> fbuild_core::Result<PathBuf> {
        let mut combined = String::new();
        let mut line_offsets: Vec<(usize, &Path)> = Vec::new();
        let mut current_line = 1;

        for ino in ino_files {
            let content = std::fs::read_to_string(ino)?;
            line_offsets.push((current_line, ino.as_path()));
            current_line += content.lines().count();
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&content);
        }

        let prototypes = extract_function_prototypes(&combined);
        let existing_decls = find_existing_forward_declarations(&combined);

        // Remove existing forward declarations from the body
        let mut body = combined.clone();
        for decl in &existing_decls {
            body = body.replace(decl, "");
        }

        // Build output
        let mut output = String::new();

        if include_arduino_h {
            output.push_str("#include <Arduino.h>\n");
        }

        // Function prototypes
        if !prototypes.is_empty() {
            output.push_str("// Auto-generated function prototypes\n");
            for proto in &prototypes {
                output.push_str(proto);
                output.push_str(";\n");
            }
            output.push('\n');
        }

        // #line directive for first file
        if let Some((_, first_file)) = line_offsets.first() {
            output.push_str(&format!(
                "#line 1 \"{}\"\n",
                first_file.display().to_string().replace('\\', "/")
            ));
        }

        output.push_str(&body);

        // Write to build directory
        std::fs::create_dir_all(&self.build_dir)?;

        // Use the first .ino file's stem for the output name
        let stem = ino_files[0]
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        let output_path = self.build_dir.join(format!("{}.ino.cpp", stem));
        write_if_changed(&output_path, &output)?;

        Ok(output_path)
    }
}

impl SourceFilter {
    fn parse(spec: Option<&str>) -> fbuild_core::Result<Self> {
        let mut rules = Vec::new();
        let mut has_include_rules = false;

        let Some(spec) = spec else {
            return Ok(Self {
                rules,
                has_include_rules,
            });
        };

        for raw in spec.lines().flat_map(|line| line.split(',')) {
            let token = raw.trim();
            if token.is_empty() {
                continue;
            }

            let (include, inner) = if token.starts_with("+<") && token.ends_with('>') {
                (true, &token[2..token.len() - 1])
            } else if token.starts_with("-<") && token.ends_with('>') {
                (false, &token[2..token.len() - 1])
            } else {
                return Err(fbuild_core::FbuildError::ConfigError(format!(
                    "invalid source filter rule '{}': expected +/-<pattern>",
                    token
                )));
            };

            let pattern = inner.trim().replace('\\', "/");
            if pattern.is_empty() {
                return Err(fbuild_core::FbuildError::ConfigError(
                    "source filter rule must not be empty".to_string(),
                ));
            }

            if include {
                has_include_rules = true;
            }

            rules.push(SourceFilterRule {
                include,
                matcher: compile_source_filter_pattern(&pattern)?,
            });
        }

        Ok(Self {
            rules,
            has_include_rules,
        })
    }

    fn matches(&self, root: &Path, path: &Path) -> bool {
        if self.rules.is_empty() {
            return true;
        }

        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        let mut included = !self.has_include_rules;
        for rule in &self.rules {
            if rule.matcher.is_match(&rel) {
                included = rule.include;
            }
        }
        included
    }
}

fn arduino_header_available(include_roots: &[&Path]) -> bool {
    include_roots
        .iter()
        .any(|root| root.join("Arduino.h").is_file())
}

fn compile_source_filter_pattern(pattern: &str) -> fbuild_core::Result<Regex> {
    let normalized = pattern.replace('\\', "/");
    let regex_body = if normalized == "*" {
        String::from(".*")
    } else if normalized.ends_with('/') {
        format!(
            "{}(?:/.*)?",
            glob_fragment_to_regex(normalized.trim_end_matches('/'))
        )
    } else if normalized.contains('/') {
        glob_fragment_to_regex(&normalized)
    } else {
        format!("(?:.*/)?{}", glob_fragment_to_regex(&normalized))
    };

    Regex::new(&format!("^{}$", regex_body)).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "invalid source filter pattern '{}': {}",
            normalized, e
        ))
    })
}

fn glob_fragment_to_regex(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    out.push_str(".*");
                    i += 1;
                } else {
                    out.push_str("[^/]*");
                }
            }
            '?' => out.push_str("[^/]"),
            c if ".+()[]{}^$|\\".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

fn write_if_changed(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == contents {
            return Ok(());
        }
    }
    std::fs::write(path, contents)
}

/// Walk a directory for source files, respecting exclude list.
/// Walk a directory tree collecting source files, skipping excluded subdirectories.
///
/// Excludes:
///   - Any directory in EXCLUDE_DIRS (build artifacts, VCS, package managers, etc.)
///   - Any hidden directory (name starts with `.`) — covers `.build`, `.cache`, etc.
///   - The walk's root is never excluded by name (to allow scanning hidden roots).
fn walk_sources(dir: &Path) -> Vec<PathBuf> {
    let exclude: HashSet<&str> = EXCLUDE_DIRS.iter().copied().collect();
    let mut files = Vec::new();

    let root = dir.to_path_buf();
    for entry in WalkDir::new(&root)
        .into_iter()
        .filter_entry(|e| {
            // Always allow the root itself (even if its name starts with '.')
            if e.path() == root {
                return true;
            }
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                if exclude.contains(name.as_ref()) {
                    return false;
                }
                // Skip hidden directories (anything starting with '.')
                if name.starts_with('.') {
                    return false;
                }
                true
            } else {
                true
            }
        })
        .flatten()
    {
        if entry.file_type().is_file() {
            files.push(entry.into_path());
        }
    }

    files.sort();
    files
}

/// Extract function prototypes from concatenated .ino source.
///
/// Finds function definitions and generates forward declarations.
pub fn extract_function_prototypes(source: &str) -> Vec<String> {
    let func_re =
        Regex::new(r"(?m)^([a-zA-Z_][\w\s\*&:<>,]*?)\s+([a-zA-Z_]\w*)\s*\(([^)]*)\)\s*\{").unwrap();

    // Keywords that look like function definitions but aren't
    let skip_keywords: HashSet<&str> = ["if", "while", "for", "switch", "catch", "else"]
        .iter()
        .copied()
        .collect();

    let mut prototypes = Vec::new();
    let mut seen = HashSet::new();

    for cap in func_re.captures_iter(source) {
        let return_type = cap[1].trim();
        let func_name = &cap[2];
        let params = cap[3].trim();

        if skip_keywords.contains(func_name) {
            continue;
        }

        // Skip if it looks like a macro or class method
        if return_type.contains('#') || return_type.contains("::") {
            continue;
        }

        let proto = format!("{} {}({})", return_type, func_name, params);
        if seen.insert(proto.clone()) {
            prototypes.push(proto);
        }
    }

    prototypes
}

/// Find existing forward declarations in source (lines ending with `);`
/// that look like function prototypes).
fn find_existing_forward_declarations(source: &str) -> Vec<String> {
    let decl_re =
        Regex::new(r"(?m)^([a-zA-Z_][\w\s\*&:<>,]*?)\s+([a-zA-Z_]\w*)\s*\([^)]*\)\s*;\s*$")
            .unwrap();

    decl_re
        .find_iter(source)
        .map(|m| m.as_str().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_project(src_files: &[(&str, &str)]) -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let build_dir = tmp.path().join("build");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&build_dir).unwrap();

        for (name, content) in src_files {
            let path = src_dir.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }

        (tmp, src_dir, build_dir)
    }

    #[test]
    fn test_scan_empty_directory() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let build_dir = tmp.path().join("build");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&build_dir).unwrap();

        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        assert!(sources.is_empty());
    }

    #[test]
    fn test_nonexistent_source_directory() {
        let tmp = TempDir::new().unwrap();
        let scanner =
            SourceScanner::new(&tmp.path().join("nonexistent"), &tmp.path().join("build"));
        let sources = scanner.scan_sketch_sources().unwrap();
        assert!(sources.is_empty());
    }

    #[test]
    fn test_scan_cpp_files() {
        let (_tmp, src_dir, build_dir) = setup_project(&[("main.cpp", "int main() { return 0; }")]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].to_string_lossy().contains("main.cpp"));
    }

    #[test]
    fn test_scan_c_files() {
        let (_tmp, src_dir, build_dir) = setup_project(&[("helper.c", "void helper() {}")]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].to_string_lossy().contains("helper.c"));
    }

    #[test]
    fn test_scan_single_ino_file() {
        let (_tmp, src_dir, build_dir) =
            setup_project(&[("sketch.ino", "void setup() {}\nvoid loop() {}\n")]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].to_string_lossy().contains(".ino.cpp"));

        // Direct sketch scans do not know framework include roots.
        let content = fs::read_to_string(&sources[0]).unwrap();
        assert!(!content.contains("#include <Arduino.h>"));
    }

    #[test]
    fn test_scan_multiple_ino_files() {
        let (_tmp, src_dir, build_dir) = setup_project(&[
            ("a.ino", "void helperA() {}\n"),
            ("b.ino", "void helperB() {}\n"),
        ]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        assert_eq!(sources.len(), 1); // Concatenated into one .cpp
        let content = fs::read_to_string(&sources[0]).unwrap();
        assert!(content.contains("helperA"));
        assert!(content.contains("helperB"));
    }

    #[test]
    fn test_scan_mixed_sources() {
        let (_tmp, src_dir, build_dir) = setup_project(&[
            ("sketch.ino", "void setup() {}\nvoid loop() {}\n"),
            ("helper.cpp", "void helper() {}"),
            ("util.c", "void util() {}"),
        ]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        assert_eq!(sources.len(), 3); // 1 preprocessed ino + 2 others
    }

    #[test]
    fn test_scan_headers() {
        let (_tmp, src_dir, build_dir) = setup_project(&[
            ("main.cpp", ""),
            ("header.h", "#pragma once"),
            ("header2.hpp", "#pragma once"),
        ]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let headers = scanner.scan_headers(&src_dir);
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn test_scan_subdirectories() {
        let (_tmp, src_dir, build_dir) = setup_project(&[
            ("main.cpp", ""),
            ("sub/helper.cpp", ""),
            ("sub/deep/util.c", ""),
        ]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        assert_eq!(sources.len(), 3);
    }

    #[test]
    fn test_scan_core_sources() {
        let tmp = TempDir::new().unwrap();
        let core_dir = tmp.path().join("cores/arduino");
        fs::create_dir_all(&core_dir).unwrap();
        fs::write(core_dir.join("main.cpp"), "int main() {}").unwrap();
        fs::write(core_dir.join("wiring.c"), "void init() {}").unwrap();
        fs::write(core_dir.join("Arduino.h"), "#pragma once").unwrap();

        let scanner = SourceScanner::new(&tmp.path().join("src"), &tmp.path().join("build"));
        let sources = scanner.scan_core_sources(&core_dir);
        assert_eq!(sources.len(), 2); // .cpp and .c, not .h
    }

    #[test]
    fn test_nonexistent_core_directory() {
        let tmp = TempDir::new().unwrap();
        let scanner = SourceScanner::new(&tmp.path().join("src"), &tmp.path().join("build"));
        let sources = scanner.scan_core_sources(&tmp.path().join("nonexistent"));
        assert!(sources.is_empty());
    }

    #[test]
    fn test_preprocess_simple_ino() {
        let (_tmp, src_dir, build_dir) = setup_project(&[(
            "sketch.ino",
            "void setup() {\n  pinMode(13, OUTPUT);\n}\n\nvoid loop() {\n  digitalWrite(13, HIGH);\n}\n",
        )]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        let content = fs::read_to_string(&sources[0]).unwrap();

        assert!(!content.contains("#include <Arduino.h>"));
        assert!(content.contains("void setup()"));
        assert!(content.contains("void loop()"));
    }

    #[test]
    fn test_preprocess_includes_arduino_h_when_header_available() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let build_dir = tmp.path().join("build");
        let core_dir = tmp.path().join("core");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&core_dir).unwrap();
        fs::write(
            src_dir.join("sketch.ino"),
            "void setup() {}\nvoid loop() {}\n",
        )
        .unwrap();
        fs::write(core_dir.join("Arduino.h"), "#pragma once\n").unwrap();

        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner
            .scan_all(Some(&core_dir), None)
            .unwrap()
            .sketch_sources;
        let content = fs::read_to_string(&sources[0]).unwrap();

        assert!(content.contains("#include <Arduino.h>"));
    }

    #[test]
    fn test_preprocess_with_custom_functions() {
        let (_tmp, src_dir, build_dir) = setup_project(&[(
            "sketch.ino",
            "int add(int a, int b) {\n  return a + b;\n}\n\nvoid setup() {\n  int x = add(1, 2);\n}\n\nvoid loop() {}\n",
        )]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        let content = fs::read_to_string(&sources[0]).unwrap();

        // Should have auto-generated prototypes
        assert!(content.contains("int add(int a, int b)"));
        assert!(content.contains("void setup()"));
    }

    #[test]
    fn test_function_prototype_extraction() {
        let source = "void setup() {\n}\nint compute(float x, int y) {\n  return 0;\n}\nconst char* getName() {\n  return \"\";\n}\n";
        let protos = extract_function_prototypes(source);
        assert!(protos.len() >= 2);
        assert!(protos.iter().any(|p| p.contains("setup")));
        assert!(protos.iter().any(|p| p.contains("compute")));
    }

    #[test]
    fn test_prototype_extraction_skips_keywords() {
        let source = "void setup() {\n  if (true) {\n  }\n  while (false) {\n  }\n}\n";
        let protos = extract_function_prototypes(source);
        assert!(!protos.iter().any(|p| p.contains("if")));
        assert!(!protos.iter().any(|p| p.contains("while")));
    }

    #[test]
    fn test_line_numbers_preserved() {
        let (_tmp, src_dir, build_dir) =
            setup_project(&[("sketch.ino", "void setup() {}\nvoid loop() {}\n")]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        let content = fs::read_to_string(&sources[0]).unwrap();
        assert!(content.contains("#line 1"));
    }

    #[test]
    fn test_preprocess_does_not_rewrite_unchanged_output() {
        let (_tmp, src_dir, build_dir) =
            setup_project(&[("sketch.ino", "void setup() {}\nvoid loop() {}\n")]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);

        let first = scanner.scan_sketch_sources().unwrap();
        let output = first[0].clone();
        let first_mtime = fs::metadata(&output).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));

        let second = scanner.scan_sketch_sources().unwrap();
        assert_eq!(second[0], output);
        let second_mtime = fs::metadata(&output).unwrap().modified().unwrap();

        assert_eq!(first_mtime, second_mtime);
    }

    #[test]
    fn test_preprocess_with_arduino_h_does_not_rewrite_unchanged_output() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let build_dir = tmp.path().join("build");
        let core_dir = tmp.path().join("core");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&core_dir).unwrap();
        fs::write(
            src_dir.join("sketch.ino"),
            "void setup() {}\nvoid loop() {}\n",
        )
        .unwrap();
        fs::write(core_dir.join("Arduino.h"), "#pragma once\n").unwrap();
        let scanner = SourceScanner::new(&src_dir, &build_dir);

        let first = scanner
            .scan_all(Some(&core_dir), None)
            .unwrap()
            .sketch_sources;
        let output = first[0].clone();
        let first_mtime = fs::metadata(&output).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));

        let second = scanner
            .scan_all(Some(&core_dir), None)
            .unwrap()
            .sketch_sources;
        assert_eq!(second[0], output);
        let second_mtime = fs::metadata(&output).unwrap().modified().unwrap();

        assert_eq!(first_mtime, second_mtime);
    }

    #[test]
    fn test_source_collection_all_sources() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let core_dir = tmp.path().join("core");
        let variant_dir = tmp.path().join("variant");
        let build_dir = tmp.path().join("build");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&core_dir).unwrap();
        fs::create_dir_all(&variant_dir).unwrap();
        fs::write(src_dir.join("main.cpp"), "").unwrap();
        fs::write(core_dir.join("core.cpp"), "").unwrap();
        fs::write(variant_dir.join("variant.c"), "").unwrap();

        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let collection = scanner
            .scan_all(Some(&core_dir), Some(&variant_dir))
            .unwrap();
        assert_eq!(collection.sketch_sources.len(), 1);
        assert_eq!(collection.core_sources.len(), 1);
        assert_eq!(collection.variant_sources.len(), 1);
        assert_eq!(collection.all_sources().len(), 3);
    }

    #[test]
    fn test_scan_sketch_sources_filtered_excludes_subdirectory() {
        let (_tmp, src_dir, build_dir) = setup_project(&[
            ("main.cpp", "int main() { return 0; }"),
            ("generated/skip.cpp", "void skip() {}"),
        ]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner
            .scan_sketch_sources_filtered(Some("+<*>\n-<generated/>"))
            .unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].ends_with("main.cpp"));
    }

    #[test]
    fn test_scan_sketch_sources_filtered_includes_only_selected_files() {
        let (_tmp, src_dir, build_dir) = setup_project(&[
            ("main.cpp", "int main() { return 0; }"),
            ("helper.cpp", "void helper() {}"),
            ("sub/util.c", "void util() {}"),
        ]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner
            .scan_sketch_sources_filtered(Some("+<main.cpp>\n+<sub/util.c>"))
            .unwrap();
        assert_eq!(sources.len(), 2);
        assert!(sources.iter().any(|p| p.ends_with("main.cpp")));
        assert!(sources
            .iter()
            .any(|p| p.ends_with("sub\\util.c") || p.ends_with("sub/util.c")));
        assert!(!sources.iter().any(|p| p.ends_with("helper.cpp")));
    }
}
