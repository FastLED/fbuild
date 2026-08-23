//! Source file scanning and .ino preprocessing.
//!
//! Finds .cpp, .cc, .cxx, .c, .S, .ino files in project source directories.
//! Preprocesses .ino files into valid .cpp with function prototypes and an
//! Arduino.h include when the active include roots provide that header.
//!
//! ## Glob-pattern separator normalization
//!
//! This module accepts user-supplied glob patterns from `platformio.ini`
//! (`src_filter`, `lib_ldf_mode`) as raw strings. Those patterns are not
//! filesystem paths yet — `NormalizedPath` is the wrong type. Instead,
//! every call site routes the pattern-level `\` → `/` rewrite through
//! `normalize_glob_separators`, the single auditable owner of that
//! transform. The workspace's `ban_manual_slash_normalize` dylint
//! allowlists this file's definition site for exactly that reason
//! (FastLED/fbuild#911).

use owo_colors::OwoColorize;
use regex::Regex;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser};
use walkdir::WalkDir;

/// Slash-normalize a user-supplied glob pattern for `platformio.ini`
/// `src_filter` / `lib_ldf_mode` matching.
///
/// The lone auditable owner of the pattern-level `\` → `/` rewrite in
/// this module. Every glob-pattern call site (`SourceFilter::parse`,
/// `SourceFilter::matches`, `compile_source_filter_pattern`,
/// `normalize_generated_source_path_text`) routes through this helper.
///
/// Do NOT hand-roll `.replace('\\', "/")` on glob strings — the
/// `ban_manual_slash_normalize` dylint flags that anti-pattern.
/// Filesystem paths use `fbuild_core::path::NormalizedPath::display_slash()`
/// instead; this helper is for glob-shape strings that aren't (yet)
/// filesystem paths.
fn normalize_glob_separators(pattern: &str) -> String {
    // Glob-pattern normalization is INTENTIONALLY unconditional
    // (unlike `NormalizedPath::display_slash()` which gates on
    // `fbuild_core::platform::host::is_windows()`) — glob patterns come from `platformio.ini` and
    // may contain a mix of `\` and `/` regardless of host OS.
    pattern.replace('\\', "/")
}

/// Raw-`.ino` path → prelude-header path pairs (FastLED/fbuild#1076 Phase 0).
/// See [`SourceCollection::ino_preludes`].
pub type InoPreludeMap = Vec<(PathBuf, PathBuf)>;

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
    /// Raw-`.ino` → prelude-header path mapping (FastLED/fbuild#1076 Phase 0).
    ///
    /// Populated only when `.ino` tabs were preprocessed (empty when the
    /// sketch has no `.ino` files, or when `main.cpp` skips preprocessing).
    /// Each prelude file holds the machine-written top half that the
    /// generated `<stem>.ino.cpp` normally carries inline (the `Arduino.h`
    /// include + extracted prototypes, plus — for non-primary tabs — the
    /// full text of every preceding tab). IDE-flavored compile-DB generation
    /// uses this to swap the generated `.ino.cpp` entry for one raw-`.ino`
    /// entry per tab with `-x c++ -include <prelude>`.
    pub ino_preludes: InoPreludeMap,
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
    fbuild_paths::FBUILD_DIR_NAME,
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
        let (sources, _ino_preludes) = self
            .scan_sketch_sources_filtered_with_include_roots_and_preludes(
                filter_spec,
                include_roots,
            )?;
        Ok(sources)
    }

    /// Same as [`Self::scan_sketch_sources_filtered_with_include_roots`] but
    /// also returns the raw-`.ino` → prelude-header mapping (see
    /// [`SourceCollection::ino_preludes`]) so IDE-flavored compile-DB
    /// generation can swap in raw-`.ino` entries (FastLED/fbuild#1076 Phase 0).
    pub fn scan_sketch_sources_filtered_with_include_roots_and_preludes(
        &self,
        filter_spec: Option<&str>,
        include_roots: &[&Path],
    ) -> fbuild_core::Result<(Vec<PathBuf>, InoPreludeMap)> {
        if !self.src_dir.exists() {
            return Ok((Vec::new(), Vec::new()));
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
        let mut ino_preludes = Vec::new();
        if !ino_files.is_empty() && main_cpp_path.is_none() {
            let ino_files = order_ino_files(&self.src_dir, ino_files);
            let (preprocessed, preludes) =
                self.preprocess_ino_files(&ino_files, arduino_header_available(include_roots))?;
            sources.insert(0, preprocessed);
            ino_preludes = preludes;
        }

        Ok((sources, ino_preludes))
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
        let (sketch_sources, ino_preludes) = self
            .scan_sketch_sources_filtered_with_include_roots_and_preludes(
                filter_spec,
                &include_roots,
            )?;
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
            ino_preludes,
        })
    }

    /// Preprocess .ino files into a single .cpp file, plus per-tab prelude
    /// headers for IDE use (FastLED/fbuild#1076 Phase 0).
    ///
    /// 1. Read + normalize every tab (primary sketch first, then tabs alphabetically)
    /// 2. Extract function prototypes from the concatenated tab text
    /// 3. Build the shared "prelude" text: `#include <Arduino.h>` (when
    ///    available) + the auto-generated prototype block
    /// 4. Emit `<build_dir>/<stem>.ino.cpp` = prelude + every tab's text, each
    ///    preceded by its own `#line 1 "<tab path>"` directive (previously
    ///    only the first tab got one — secondary-tab compile errors reported
    ///    the wrong file/line)
    /// 5. Emit prelude header(s) alongside it via [`Self::write_ino_preludes`]
    ///
    /// Returns the generated `.ino.cpp` path and the raw-`.ino` → prelude
    /// path mapping.
    fn preprocess_ino_files(
        &self,
        ino_files: &[PathBuf],
        include_arduino_h: bool,
    ) -> fbuild_core::Result<(PathBuf, InoPreludeMap)> {
        let contents: Vec<String> = ino_files
            .iter()
            .map(|ino| -> fbuild_core::Result<String> {
                Ok(normalize_generated_source_line_endings(
                    &std::fs::read_to_string(ino)?,
                ))
            })
            .collect::<fbuild_core::Result<Vec<_>>>()?;

        // Hoist every tab's `#include` directives into the prelude so that
        // auto-generated prototypes can reference types from library headers
        // (FastLED/fbuild#1275 — arduino-cli places prototypes *after* the
        // include block for exactly this reason).  Replace each hoisted
        // `#include` line with a blank line in the body to preserve line
        // numbering for diagnostics.
        let sketch_includes = hoist_include_directives(&contents);
        let stripped_contents: Vec<String> = contents
            .iter()
            .map(|c| strip_include_directives(c))
            .collect();

        // Prototype extraction needs to see every tab's code, so it operates
        // on the plain concatenation (no #line noise).  We feed it the
        // *original* (pre-strip) contents so the tree-sitter parse still sees
        // the full sketch — the `#include` lines are preprocessor noise that
        // tree-sitter skips anyway.
        let combined_for_prototypes = contents.join("\n");
        let prototypes = extract_function_prototypes(&combined_for_prototypes);

        let prelude = self.build_ino_prelude(include_arduino_h, &sketch_includes, &prototypes);

        // Body: every tab's own text with a `#line` directive at each
        // boundary, so diagnostics in any tab — not just the first — map
        // back to the right file/line.
        let mut body = String::new();
        for (ino, content) in ino_files.iter().zip(stripped_contents.iter()) {
            body.push_str(&format!("#line 1 \"{}\"\n", self.line_directive_path(ino)));
            body.push_str(content);
            if !content.ends_with('\n') {
                body.push('\n');
            }
        }

        let output = format!("{prelude}{body}");

        // Write to build directory
        std::fs::create_dir_all(&self.build_dir)?;

        // Use the first .ino file's stem for the output name
        let stem = ino_files[0]
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let output_path = self.build_dir.join(format!("{}.ino.cpp", stem));
        write_if_changed(&output_path, &output)?;

        let ino_preludes = self.write_ino_preludes(ino_files, &stripped_contents, &prelude)?;

        Ok((output_path, ino_preludes))
    }

    /// Build the shared prelude text: `#include <Arduino.h>` (when
    /// available) + sketch `#include` directives (hoisted from the tab
    /// bodies — FastLED/fbuild#1275) + the auto-generated prototype block.
    /// This is exactly the machine-written top half the generated `.ino.cpp`
    /// carries inline.
    fn build_ino_prelude(
        &self,
        include_arduino_h: bool,
        sketch_includes: &[String],
        prototypes: &[String],
    ) -> String {
        let mut prelude = String::new();
        if include_arduino_h {
            prelude.push_str("#include <Arduino.h>\n");
        }
        for inc in sketch_includes {
            prelude.push_str(inc);
            prelude.push('\n');
        }
        if !prototypes.is_empty() {
            prelude.push_str("// Auto-generated function prototypes\n");
            for proto in prototypes {
                prelude.push_str(proto);
                prelude.push_str(";\n");
            }
            prelude.push('\n');
        }
        prelude
    }

    /// Emit prelude header(s) so clangd can give the raw `.ino` files
    /// first-class IntelliSense (FastLED/fbuild#1076 Phase 0 — direction
    /// update: "use the converter's `.ino.cpp` output for clangd
    /// IntelliSense").
    ///
    /// - Single tab: `<build_dir>/<stem>.ino.prelude.h` = exactly the shared
    ///   prelude text, so `prelude + "#line 1 ..." + sketch text` is
    ///   byte-identical to the generated `.ino.cpp`.
    /// - Multi-tab: one prelude per tab, `<build_dir>/<tab_stem>.ino.prelude.h`
    ///   = shared prelude + the full text of every tab that precedes this tab
    ///   in build order, each preceded by its own `#line` directive — i.e.
    ///   exactly what that tab would see in the real concatenated build.
    ///
    /// Returns the raw-`.ino` → prelude-path mapping in tab order.
    fn write_ino_preludes(
        &self,
        ino_files: &[PathBuf],
        contents: &[String],
        prelude: &str,
    ) -> fbuild_core::Result<InoPreludeMap> {
        let mut mapping = Vec::with_capacity(ino_files.len());

        for (i, ino) in ino_files.iter().enumerate() {
            let mut tab_prelude = prelude.to_string();
            for (prior_ino, prior_content) in ino_files[..i].iter().zip(contents[..i].iter()) {
                tab_prelude.push_str(&format!(
                    "#line 1 \"{}\"\n",
                    self.line_directive_path(prior_ino)
                ));
                tab_prelude.push_str(prior_content);
                if !prior_content.ends_with('\n') {
                    tab_prelude.push('\n');
                }
            }

            let tab_stem = ino.file_stem().unwrap_or_default().to_string_lossy();
            let prelude_path = self.build_dir.join(format!("{}.ino.prelude.h", tab_stem));
            write_if_changed(&prelude_path, &tab_prelude)?;
            mapping.push((ino.clone(), prelude_path));
        }

        Ok(mapping)
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

            let pattern = normalize_glob_separators(inner.trim());
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

        let rel =
            normalize_glob_separators(&path.strip_prefix(root).unwrap_or(path).to_string_lossy());

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
    let mut normalized = normalize_glob_separators(path);
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
    let normalized = normalize_glob_separators(pattern);
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

/// Extract every `#include` directive across all tabs, deduplicated and in
/// first-seen order, for hoisting into the prelude (FastLED/fbuild#1275).
fn hoist_include_directives(contents: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut includes = Vec::new();
    for content in contents {
        for line in content.lines() {
            let trimmed = line.trim();
            if is_include_directive(trimmed) {
                let normalized = trimmed.to_string();
                if seen.insert(normalized.clone()) {
                    includes.push(normalized);
                }
            }
        }
    }
    includes
}

/// Replace every `#include` line with an empty line so that line numbering
/// is preserved when the hoisted directives are moved to the prelude
/// (FastLED/fbuild#1275).
fn strip_include_directives(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            if is_include_directive(line.trim()) {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_include_directive(trimmed: &str) -> bool {
    trimmed.starts_with("#include")
}

/// Extract function prototypes from concatenated .ino source using a C++ parser.
///
/// Skips any prototype whose return type or parameter types reference a
/// type declared elsewhere in `source` (struct/class/union/enum, `typedef`,
/// or `using` alias) — see `collect_sketch_defined_type_names` and
/// `signature_references_sketch_type` (both private) for FastLED/fbuild#1196's
/// rationale:
/// emitting such a prototype at the top of the file (ahead of the type
/// declaration) is *always* a compile error, so skipping it just means the
/// function falls back to ordinary declare-before-use — exactly the
/// documented arduino-cli workaround, minus the error.
pub fn extract_function_prototypes(source: &str) -> Vec<String> {
    let Some(tree) = parse_cpp_source(source) else {
        return Vec::new();
    };

    let sketch_types = collect_sketch_defined_type_names(tree.root_node(), source);

    let mut raw_prototypes = Vec::new();
    collect_function_prototypes(tree.root_node(), source, &sketch_types, &mut raw_prototypes);
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

fn collect_function_prototypes(
    node: Node<'_>,
    source: &str,
    sketch_types: &HashSet<String>,
    prototypes: &mut Vec<String>,
) {
    if node.kind() == "function_definition" {
        if let Some(prototype) = prototype_from_function_definition(node, source, sketch_types) {
            prototypes.push(prototype);
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_function_prototypes(child, source, sketch_types, prototypes);
    }
}

/// Collect the names of every type the sketch itself declares:
/// `struct`/`class`/`union`/`enum` names, `typedef` target names, and
/// `using X = ...` alias names. Operates on the root of the *combined*
/// (multi-tab-concatenated) parse tree, so a type declared in one tab
/// suppresses prototypes referencing it from any other tab.
fn collect_sketch_defined_type_names(node: Node<'_>, source: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_sketch_defined_type_names_into(node, source, &mut names);
    names
}

fn collect_sketch_defined_type_names_into(
    node: Node<'_>,
    source: &str,
    names: &mut HashSet<String>,
) {
    match node.kind() {
        "struct_specifier" | "class_specifier" | "union_specifier" | "enum_specifier" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                collect_type_identifiers(name_node, source, names);
            }
        }
        "type_definition" => {
            let mut cursor = node.walk();
            for declarator in node.children_by_field_name("declarator", &mut cursor) {
                if let Some(name) = declared_type_identifier_name(declarator, source) {
                    names.insert(name);
                }
            }
        }
        "alias_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                collect_type_identifiers(name_node, source, names);
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_sketch_defined_type_names_into(child, source, names);
    }
}

/// Unwrap declarator wrapper nodes (`pointer_declarator`, `array_declarator`,
/// `function_declarator`, etc.) down to the bare `type_identifier` naming
/// the declared type, without descending into a `function_declarator`'s
/// `parameters` field (that subtree names *other* types, not this one).
fn declared_type_identifier_name(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" => Some(node_text(node, source)),
        "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "function_declarator"
        | "parenthesized_declarator"
        | "attributed_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|child| declared_type_identifier_name(child, source)),
        _ => None,
    }
}

/// Collect every `type_identifier` token's text within a subtree. Used both
/// to read struct/class/union/enum/using names (where the name field may be
/// a `template_type` wrapping a `type_identifier`) and, in
/// [`signature_references_sketch_type`], to scan a function's return/parameter
/// *type* subtrees for references to sketch-defined types.
fn collect_type_identifiers(node: Node<'_>, source: &str, out: &mut HashSet<String>) {
    if node.kind() == "type_identifier" {
        out.insert(node_text(node, source));
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_type_identifiers(child, source, out);
    }
}

fn node_text(node: Node<'_>, source: &str) -> String {
    source
        .get(node.start_byte()..node.end_byte())
        .unwrap_or_default()
        .to_string()
}

/// Whether a function's return type or any parameter type references one of
/// `sketch_types`.
///
/// Type-position based (not substring/token matching on the rendered
/// signature): walks only the `type` field of the function's declarator
/// (the return type) and the `type` field of each `parameter_declaration`
/// / `optional_parameter_declaration` in its `parameter_list`. This avoids
/// false positives from parameter *names* that happen to collide with a
/// declared type name (e.g. `void ok(int myStruct)` keeps its prototype
/// even if a `MyStruct` type exists elsewhere, since `myStruct` never
/// appears in a type position and the identifiers differ in case anyway).
fn signature_references_sketch_type(
    node: Node<'_>,
    source: &str,
    sketch_types: &HashSet<String>,
) -> bool {
    if sketch_types.is_empty() {
        return false;
    }

    let mut referenced = HashSet::new();
    if let Some(return_type) = node.child_by_field_name("type") {
        collect_type_identifiers(return_type, source, &mut referenced);
    }
    if let Some(parameter_list) = find_descendant_kind(node, "parameter_list") {
        let mut cursor = parameter_list.walk();
        for param in parameter_list.children(&mut cursor) {
            if matches!(
                param.kind(),
                "parameter_declaration" | "optional_parameter_declaration"
            ) {
                if let Some(param_type) = param.child_by_field_name("type") {
                    collect_type_identifiers(param_type, source, &mut referenced);
                }
            }
        }
    }

    referenced.iter().any(|name| sketch_types.contains(name))
}

fn prototype_from_function_definition(
    node: Node<'_>,
    source: &str,
    sketch_types: &HashSet<String>,
) -> Option<String> {
    if has_skipped_function_context(node) {
        return None;
    }
    if signature_references_sketch_type(node, source, sketch_types) {
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
