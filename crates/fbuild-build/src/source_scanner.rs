//! Source file scanning and .ino preprocessing.
//!
//! Finds .cpp, .cc, .cxx, .c, .S, .ino files in project source directories.
//! Preprocesses .ino files into valid .cpp with function prototypes and an
//! Arduino.h include when the active include roots provide that header.

use owo_colors::OwoColorize;
use regex::Regex;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser};
use walkdir::WalkDir;

/// Collection of source files found by the scanner.
#[derive(Debug, Default)]
pub struct SourceCollection {
    /// User sketch sources (.cpp/.cc/.cxx, .c, .S — and preprocessed .ino)
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
    /// Returns preprocessed .ino files as .cpp, plus existing .cpp/.cc/.cxx/.c/.S files.
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
        let mut main_cpp_path = None;
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
                "cpp" | "c" | "s" | "cc" | "cxx" => {
                    if entry.file_name().is_some_and(|n| n == "main.cpp") {
                        main_cpp_path = Some(entry.clone());
                    }
                    sources.push(entry);
                }
                _ => {}
            }
        }

        if let Some(main_cpp) = main_cpp_path.as_deref() {
            emit_main_cpp_skips_ino_warning(main_cpp, &ino_files);
        }

        // If main.cpp exists, skip preprocessing to avoid duplicate symbols when
        // the .ino content is already compiled via #include in main.cpp.
        if !ino_files.is_empty() && main_cpp_path.is_none() {
            let ino_files = order_ino_files(&self.src_dir, ino_files);
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
                matches!(ext.as_str(), "cpp" | "c" | "s" | "cc" | "cxx")
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
                matches!(ext.as_str(), "cpp" | "c" | "s" | "cc" | "cxx")
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
    /// 1. Concatenate .ino files (primary sketch first, then tabs alphabetically)
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
            let content = normalize_generated_source_line_endings(&std::fs::read_to_string(ino)?);
            line_offsets.push((current_line, ino.as_path()));
            current_line += content.lines().count();
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&content);
        }

        let prototypes = extract_function_prototypes(&combined);

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
                self.line_directive_path(first_file)
            ));
        }

        output.push_str(&combined);

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

    fn line_directive_path(&self, path: &Path) -> String {
        let project_root = self.src_dir.parent().unwrap_or(&self.src_dir);
        let display_path = path.strip_prefix(project_root).unwrap_or(path);
        normalize_generated_source_path(display_path)
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

fn emit_main_cpp_skips_ino_warning(main_cpp: &Path, ino_files: &[PathBuf]) {
    let mut stderr = io::stderr().lock();
    let _ = write_main_cpp_skips_ino_warning(&mut stderr, main_cpp, ino_files);
}

fn write_main_cpp_skips_ino_warning(
    out: &mut impl Write,
    main_cpp: &Path,
    ino_files: &[PathBuf],
) -> io::Result<()> {
    if ino_files.is_empty() {
        return Ok(());
    }

    let prefix = "warning:".bold().yellow().to_string();
    let skipped = ino_files
        .iter()
        .map(|path| normalize_generated_source_path(path))
        .collect::<Vec<_>>()
        .join(", ");
    let message = format!(
        "{} takes precedence; skipping automatic .ino preprocessing for: {}",
        normalize_generated_source_path(main_cpp),
        skipped
    )
    .yellow()
    .to_string();
    writeln!(out, "{prefix} {message}")
}

fn normalize_generated_source_path(path: &Path) -> String {
    normalize_generated_source_path_text(&path.display().to_string())
}

fn normalize_generated_source_path_text(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    let bytes = normalized.as_bytes();
    if bytes.len() >= 3
        && bytes[1] == b':'
        && bytes[2] == b'/'
        && bytes[0].is_ascii_alphabetic()
        && bytes[0].is_ascii_uppercase()
    {
        normalized.replace_range(0..1, &normalized[0..1].to_ascii_lowercase());
    }
    normalized
}

fn normalize_generated_source_line_endings(source: &str) -> String {
    source.replace("\r\n", "\n").replace('\r', "\n")
}

fn order_ino_files(src_dir: &Path, mut ino_files: Vec<PathBuf>) -> Vec<PathBuf> {
    ino_files.sort_by(|a, b| compare_ino_paths(a, b));

    if let Some(primary_index) = find_primary_ino_index(src_dir, &ino_files) {
        let primary = ino_files.remove(primary_index);
        ino_files.insert(0, primary);
    }

    ino_files
}

fn find_primary_ino_index(src_dir: &Path, ino_files: &[PathBuf]) -> Option<usize> {
    for primary_stem in primary_ino_stems(src_dir) {
        if let Some(index) = ino_files
            .iter()
            .position(|path| file_stem_eq_ignore_ascii_case(path, &primary_stem))
        {
            return Some(index);
        }
    }

    let setup_or_loop = Regex::new(r"(?m)\bvoid\s+(setup|loop)\s*\(").expect("valid regex");
    ino_files.iter().position(|path| {
        std::fs::read_to_string(path)
            .map(|content| setup_or_loop.is_match(&content))
            .unwrap_or(false)
    })
}

fn primary_ino_stems(src_dir: &Path) -> Vec<String> {
    let src_name = src_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string());

    let mut stems = Vec::new();
    if let Some(src_name) = src_name {
        if src_name.eq_ignore_ascii_case("src") {
            stems.push("main".to_string());
            if let Some(project_name) = src_dir
                .parent()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().to_string())
            {
                stems.push(project_name);
            }
        } else {
            stems.push(src_name);
        }
    }

    stems
}

fn file_stem_eq_ignore_ascii_case(path: &Path, expected: &str) -> bool {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

fn compare_ino_paths(a: &Path, b: &Path) -> Ordering {
    let a_name = file_name_for_sort(a);
    let b_name = file_name_for_sort(b);
    a_name
        .to_ascii_lowercase()
        .cmp(&b_name.to_ascii_lowercase())
        .then_with(|| a_name.cmp(&b_name))
}

fn file_name_for_sort(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
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

/// Extract function prototypes from concatenated .ino source using a C++ parser.
pub fn extract_function_prototypes(source: &str) -> Vec<String> {
    let Some(tree) = parse_cpp_source(source) else {
        return Vec::new();
    };

    let mut raw_prototypes = Vec::new();
    collect_function_prototypes(tree.root_node(), source, &mut raw_prototypes);
    let mut seen = HashSet::new();
    raw_prototypes
        .into_iter()
        .filter(|proto| seen.insert(proto.clone()))
        .collect()
}

fn parse_cpp_source(source: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    let language = tree_sitter_cpp::LANGUAGE.into();
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}

fn collect_function_prototypes(node: Node<'_>, source: &str, prototypes: &mut Vec<String>) {
    if node.kind() == "function_definition" {
        if let Some(prototype) = prototype_from_function_definition(node, source) {
            prototypes.push(prototype);
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_function_prototypes(child, source, prototypes);
    }
}

fn prototype_from_function_definition(node: Node<'_>, source: &str) -> Option<String> {
    if has_skipped_function_context(node) {
        return None;
    }

    let signature_node = node
        .parent()
        .filter(|parent| parent.kind() == "template_declaration")
        .unwrap_or(node);
    let body = node.child_by_field_name("body")?;
    let signature_start = signature_node.start_byte();
    let signature = source.get(signature_start..body.start_byte())?;
    let parameter_list = find_descendant_kind(node, "parameter_list")?;
    let params_start = parameter_list.start_byte().checked_sub(signature_start)?;
    let params_end = parameter_list.end_byte().checked_sub(signature_start)?;
    let signature = strip_default_arguments(signature, params_start, params_end);
    let signature = normalize_signature(&signature)?;

    if signature.contains("::") || signature.starts_with('#') {
        return None;
    }
    if is_arduino_entry_point_signature(&signature) {
        return None;
    }

    Some(signature)
}

fn is_arduino_entry_point_signature(signature: &str) -> bool {
    matches!(
        signature.trim(),
        "void setup()" | "void setup(void)" | "void loop()" | "void loop(void)"
    )
}

fn has_skipped_function_context(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "namespace_definition"
            | "class_specifier"
            | "struct_specifier"
            | "union_specifier"
            | "field_declaration_list" => return true,
            _ => current = parent.parent(),
        }
    }
    false
}

fn find_descendant_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return Some(child);
        }
        if let Some(found) = find_descendant_kind(child, kind) {
            return Some(found);
        }
    }
    None
}

fn normalize_signature(signature: &str) -> Option<String> {
    let lines: Vec<&str> = signature
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines.join(" "))
}

fn strip_default_arguments(signature: &str, params_start: usize, params_end: usize) -> String {
    let Some(params) = signature.get(params_start..params_end) else {
        return signature.to_string();
    };
    let Some(params_inner) = params.strip_prefix('(').and_then(|p| p.strip_suffix(')')) else {
        return signature.to_string();
    };

    let mut output = String::new();
    output.push_str(&signature[..params_start + 1]);
    output.push_str(&strip_defaults_from_params(params_inner));
    output.push_str(&signature[params_end - 1..]);
    output
}

fn strip_defaults_from_params(params: &str) -> String {
    let mut output = String::new();
    let mut skip_default = false;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut angle_depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for ch in params.chars() {
        if let Some(quote_char) = quote {
            if !skip_default {
                output.push(ch);
            }
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote_char {
                quote = None;
            }
            continue;
        }

        match ch {
            '"' | '\'' => {
                if !skip_default {
                    output.push(ch);
                }
                quote = Some(ch);
            }
            '(' => {
                paren_depth += 1;
                if !skip_default {
                    output.push(ch);
                }
            }
            ')' => {
                paren_depth = paren_depth.saturating_sub(1);
                if !skip_default {
                    output.push(ch);
                }
            }
            '[' => {
                bracket_depth += 1;
                if !skip_default {
                    output.push(ch);
                }
            }
            ']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                if !skip_default {
                    output.push(ch);
                }
            }
            '{' => {
                brace_depth += 1;
                if !skip_default {
                    output.push(ch);
                }
            }
            '}' => {
                brace_depth = brace_depth.saturating_sub(1);
                if !skip_default {
                    output.push(ch);
                }
            }
            '<' => {
                angle_depth += 1;
                if !skip_default {
                    output.push(ch);
                }
            }
            '>' => {
                angle_depth = angle_depth.saturating_sub(1);
                if !skip_default {
                    output.push(ch);
                }
            }
            '=' if paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
                && angle_depth == 0 =>
            {
                skip_default = true;
                trim_trailing_spaces(&mut output);
            }
            ',' if paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
                && angle_depth == 0 =>
            {
                skip_default = false;
                trim_trailing_spaces(&mut output);
                output.push(ch);
            }
            _ => {
                if !skip_default {
                    output.push(ch);
                }
            }
        }
    }

    trim_trailing_spaces(&mut output);
    output
}

fn trim_trailing_spaces(text: &mut String) {
    while text.chars().last().is_some_and(char::is_whitespace) {
        text.pop();
    }
}

#[cfg(test)]
mod tests;
