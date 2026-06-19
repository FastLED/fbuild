//! Source file scanning and .ino preprocessing.
//!
//! Finds .cpp, .c, .S, .ino files in project source directories.
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
                "cpp" | "c" | "s" | "cc" => {
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
                self.line_directive_path(first_file)
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

/// Find existing forward declarations in source.
fn find_existing_forward_declarations(source: &str) -> Vec<String> {
    let Some(tree) = parse_cpp_source(source) else {
        return Vec::new();
    };

    let mut declarations = Vec::new();
    collect_forward_declarations(tree.root_node(), source, &mut declarations);
    declarations
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

    Some(signature)
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

fn collect_forward_declarations(node: Node<'_>, source: &str, declarations: &mut Vec<String>) {
    if node.kind() == "declaration"
        && !has_skipped_function_context(node)
        && has_descendant_kind(node, "function_declarator")
    {
        if let Some(text) = source.get(node.start_byte()..node.end_byte()) {
            let declaration = text.trim();
            if declaration.ends_with(';') && !declaration.contains("::") {
                declarations.push(declaration.to_string());
            }
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_forward_declarations(child, source, declarations);
    }
}

fn has_descendant_kind(node: Node<'_>, kind: &str) -> bool {
    find_descendant_kind(node, kind).is_some()
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
    fn test_scan_multiple_ino_files_uses_platformio_main_first() {
        let (_tmp, src_dir, build_dir) = setup_project(&[
            ("z_tab.ino", "void zTab() {}\n"),
            ("main.ino", "void setup() {}\nvoid loop() {}\n"),
            ("a_tab.ino", "void aTab() {}\n"),
        ]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);

        let sources = scanner.scan_sketch_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].ends_with("main.ino.cpp"));
        let content = fs::read_to_string(&sources[0]).unwrap();

        let main_pos = content.find("void setup()").unwrap();
        let a_pos = content.find("void aTab()").unwrap();
        let z_pos = content.find("void zTab()").unwrap();
        assert!(main_pos < a_pos);
        assert!(a_pos < z_pos);
    }

    #[test]
    fn test_scan_multiple_ino_files_uses_arduino_named_primary_first() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("Blink");
        let build_dir = tmp.path().join("build");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&build_dir).unwrap();
        fs::write(src_dir.join("z_tab.ino"), "void zTab() {}\n").unwrap();
        fs::write(
            src_dir.join("Blink.ino"),
            "void setup() {}\nvoid loop() {}\n",
        )
        .unwrap();
        fs::write(src_dir.join("a_tab.ino"), "void aTab() {}\n").unwrap();
        let scanner = SourceScanner::new(&src_dir, &build_dir);

        let sources = scanner.scan_sketch_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].ends_with("Blink.ino.cpp"));
        let content = fs::read_to_string(&sources[0]).unwrap();

        let primary_pos = content.find("void setup()").unwrap();
        let a_pos = content.find("void aTab()").unwrap();
        let z_pos = content.find("void zTab()").unwrap();
        assert!(primary_pos < a_pos);
        assert!(a_pos < z_pos);
    }

    #[test]
    fn test_scan_multiple_ino_files_falls_back_to_setup_loop_primary() {
        let (_tmp, src_dir, build_dir) = setup_project(&[
            ("a_tab.ino", "void aTab() {}\n"),
            ("z_entry.ino", "void setup() {}\nvoid loop() {}\n"),
            ("b_tab.ino", "void bTab() {}\n"),
        ]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);

        let sources = scanner.scan_sketch_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].ends_with("z_entry.ino.cpp"));
        let content = fs::read_to_string(&sources[0]).unwrap();

        let primary_pos = content.find("void setup()").unwrap();
        let a_pos = content.find("void aTab()").unwrap();
        let b_pos = content.find("void bTab()").unwrap();
        assert!(primary_pos < a_pos);
        assert!(a_pos < b_pos);
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
    fn test_scan_main_cpp_with_ino_skips_preprocessing_but_keeps_main_cpp() {
        let (_tmp, src_dir, build_dir) = setup_project(&[
            ("main.cpp", "#include \"sketch.ino\"\n"),
            ("sketch.ino", "void setup() {}\nvoid loop() {}\n"),
        ]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);

        let sources = scanner.scan_sketch_sources().unwrap();

        assert_eq!(sources.len(), 1);
        assert!(sources[0].ends_with("main.cpp"));
        assert!(!build_dir.join("sketch.ino.cpp").exists());
    }

    #[test]
    fn test_main_cpp_with_ino_warning_is_yellow_and_clear() {
        let tmp = TempDir::new().unwrap();
        let main_cpp = tmp.path().join("src").join("main.cpp");
        let ino = tmp.path().join("src").join("sketch.ino");
        let mut out = Vec::new();

        write_main_cpp_skips_ino_warning(&mut out, &main_cpp, &[ino]).unwrap();
        let warning = String::from_utf8(out).unwrap();

        assert!(warning.contains("\u{1b}["));
        assert!(warning.contains("warning:"));
        assert!(warning.contains("main.cpp takes precedence"));
        assert!(warning.contains("skipping automatic .ino preprocessing"));
        assert!(warning.contains("sketch.ino"));
    }

    #[test]
    fn test_main_cpp_without_ino_warning_is_silent() {
        let tmp = TempDir::new().unwrap();
        let main_cpp = tmp.path().join("src").join("main.cpp");
        let mut out = Vec::new();

        write_main_cpp_skips_ino_warning(&mut out, &main_cpp, &[]).unwrap();

        assert!(out.is_empty());
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
    fn test_prototype_extraction_handles_complex_cpp_signatures() {
        let source = r#"
template <typename T>
T twice(T value) {
  return value + value;
}

[[nodiscard]] const char* label(const char* fallback = "demo") {
  return fallback;
}

int& ref_value(int& value) {
  return value;
}
"#;
        let protos = extract_function_prototypes(source);
        assert!(protos.contains(&"template <typename T> T twice(T value)".to_string()));
        assert!(
            protos.contains(&"[[nodiscard]] const char* label(const char* fallback)".to_string())
        );
        assert!(protos.contains(&"int& ref_value(int& value)".to_string()));
        assert!(!protos.iter().any(|p| p.contains("= \"demo\"")));
    }

    #[test]
    fn test_prototype_extraction_skips_non_free_functions() {
        let source = r#"
#define MAKE_FUNC(name) void name() {}

void setup() {
  if (true) {
  }
  while (false) {
  }
  auto callback = []() { return 1; };
}

class Controller {
  void tick() {}
};

namespace hidden {
void helper() {}
}

void Controller::external_tick() {}
"#;
        let protos = extract_function_prototypes(source);
        assert!(protos.iter().any(|p| p == "void setup()"));
        assert!(!protos.iter().any(|p| p.contains("if")));
        assert!(!protos.iter().any(|p| p.contains("while")));
        assert!(!protos.iter().any(|p| p.contains("callback")));
        assert!(!protos.iter().any(|p| p.contains("tick")));
        assert!(!protos.iter().any(|p| p.contains("helper")));
        assert!(!protos.iter().any(|p| p.contains("MAKE_FUNC")));
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
    fn test_line_directive_path_is_project_relative_and_slash_normalized() {
        let (_tmp, src_dir, build_dir) =
            setup_project(&[("sketch.ino", "void setup() {}\nvoid loop() {}\n")]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        let content = fs::read_to_string(&sources[0]).unwrap();

        assert!(content.contains("#line 1 \"src/sketch.ino\""));
        assert!(!content.contains('\\'));
    }

    #[test]
    fn test_generated_ino_cpp_uses_lf_line_endings() {
        let (_tmp, src_dir, build_dir) =
            setup_project(&[("sketch.ino", "void setup() {}\r\nvoid loop() {}\r\n")]);
        let scanner = SourceScanner::new(&src_dir, &build_dir);
        let sources = scanner.scan_sketch_sources().unwrap();
        let content = fs::read_to_string(&sources[0]).unwrap();

        assert!(!content.contains("\r\n"));
    }

    #[test]
    fn test_windows_style_generated_path_text_is_stable() {
        assert_eq!(
            normalize_generated_source_path_text(r"C:\Users\dev\project\src\main.ino"),
            "c:/Users/dev/project/src/main.ino"
        );
        assert_eq!(
            normalize_generated_source_path_text(r"C:\Users\dev\project/src\main.ino"),
            "c:/Users/dev/project/src/main.ino"
        );
        assert_eq!(
            normalize_generated_source_path_text("src\\main.ino"),
            "src/main.ino"
        );
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
