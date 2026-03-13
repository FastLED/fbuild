//! Source file scanning and .ino preprocessing.
//!
//! Finds .cpp, .c, .S, .ino files in project source directories.
//! Preprocesses .ino files into valid .cpp with Arduino.h include and function prototypes.

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
    "build",
    ".git",
    "__pycache__",
    "node_modules",
    ".fbuild",
];

/// Scans project directories for source files and preprocesses .ino files.
pub struct SourceScanner {
    /// Project source directory (usually <project>/src)
    src_dir: PathBuf,
    /// Build output directory (for preprocessed .ino → .cpp)
    build_dir: PathBuf,
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
    pub fn scan_sketch_sources(&self) -> fbuild_core::Result<Vec<PathBuf>> {
        if !self.src_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sources = Vec::new();
        let mut ino_files = Vec::new();

        for entry in walk_sources(&self.src_dir) {
            let ext = entry
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            match ext.as_str() {
                "ino" => ino_files.push(entry),
                "cpp" | "c" | "s" | "cc" => sources.push(entry),
                _ => {}
            }
        }

        // Preprocess .ino files
        if !ino_files.is_empty() {
            ino_files.sort();
            let preprocessed = self.preprocess_ino_files(&ino_files)?;
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
        let sketch_sources = self.scan_sketch_sources()?;
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
    /// 2. Add `#include <Arduino.h>` at top
    /// 3. Extract function prototypes
    /// 4. Add prototypes before first function definition
    /// 5. Add `#line` directives for debugging
    fn preprocess_ino_files(&self, ino_files: &[PathBuf]) -> fbuild_core::Result<PathBuf> {
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

        // Arduino.h include
        output.push_str("#include <Arduino.h>\n");

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
        std::fs::write(&output_path, &output)?;

        Ok(output_path)
    }
}

/// Walk a directory for source files, respecting exclude list.
fn walk_sources(dir: &Path) -> Vec<PathBuf> {
    let exclude: HashSet<&str> = EXCLUDE_DIRS.iter().copied().collect();
    let mut files = Vec::new();

    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                !exclude.contains(name.as_ref())
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

        // Check preprocessed content
        let content = fs::read_to_string(&sources[0]).unwrap();
        assert!(content.contains("#include <Arduino.h>"));
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

        assert!(content.contains("#include <Arduino.h>"));
        assert!(content.contains("void setup()"));
        assert!(content.contains("void loop()"));
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
}
