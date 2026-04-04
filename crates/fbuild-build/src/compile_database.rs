//! Compile database (`compile_commands.json`) generation for IDE support.
//!
//! Generates a JSON compilation database (clangd-compatible) so that
//! "Go to Definition" and other IDE features work with real include paths
//! instead of response file (`@file`) references.

use std::path::{Path, PathBuf};

use fbuild_core::Result;

/// A single entry in the compile database.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompileEntry {
    /// The compiler invocation as an argument list (preferred by clangd).
    pub arguments: Vec<String>,
    /// The working directory for the compilation.
    pub directory: String,
    /// The source file being compiled.
    pub file: String,
    /// The output object file (optional per spec).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// Container for compile database entries.
pub struct CompileDatabase {
    entries: Vec<CompileEntry>,
}

impl Default for CompileDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl CompileDatabase {
    /// Create an empty compile database.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Add an entry to the database.
    pub fn add_entry(&mut self, entry: CompileEntry) {
        self.entries.push(entry);
    }

    /// Add multiple entries to the database.
    pub fn extend(&mut self, entries: Vec<CompileEntry>) {
        self.entries.extend(entries);
    }

    /// Whether the database has any entries.
    pub fn has_entries(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Write `compile_commands.json` to the given directory.
    pub fn write(&self, dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(dir).map_err(|e| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to create directory {}: {}",
                dir.display(),
                e
            ))
        })?;

        let path = dir.join("compile_commands.json");
        let json = serde_json::to_string_pretty(&self.entries).map_err(|e| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to serialize compile database: {}",
                e
            ))
        })?;

        std::fs::write(&path, json).map_err(|e| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to write {}: {}",
                path.display(),
                e
            ))
        })?;

        Ok(path)
    }

    /// Write to `build_dir` and copy to `project_dir` (matching Python fbuild behavior).
    ///
    /// If the project has a `library.json` at its root (indicating it IS a library,
    /// e.g. FastLED), the copy to project root is suppressed to avoid overwriting
    /// a meson/cmake-generated `compile_commands.json` that uses correct source paths.
    pub fn write_and_copy(&self, build_dir: &Path, project_dir: &Path) -> Result<PathBuf> {
        let build_path = self.write(build_dir)?;

        if is_library_project(project_dir) {
            tracing::info!(
                "library.json detected — skipping compile_commands.json copy to project root"
            );
            return Ok(build_path);
        }

        let project_path = self.write(project_dir)?;
        Ok(project_path)
    }
}

/// Strip cache wrapper (sccache/zccache/ccache) from compiler arguments.
///
/// If the first element of `args` is a known cache wrapper, returns args
/// without it (the real compiler is the second element). Otherwise returns
/// args unchanged.
pub fn strip_cache_wrapper(args: &[String]) -> Vec<String> {
    if args.len() < 2 {
        return args.to_vec();
    }

    // Extract the file stem manually so Windows paths (with `\`) work on Unix.
    // `Path::file_stem` only splits on the platform's native separator, so
    // `C:\...\sccache.exe` is treated as one component on Linux/macOS.
    let filename = args[0].rsplit(['/', '\\']).next().unwrap_or(&args[0]);
    let stem = filename
        .strip_suffix(".exe")
        .or_else(|| filename.strip_suffix(".EXE"))
        .unwrap_or(filename)
        .to_lowercase();

    if stem == "sccache" || stem == "ccache" || stem == "zccache" {
        args[1..].to_vec()
    } else {
        args.to_vec()
    }
}

/// Target architecture for clang flag translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetArchitecture {
    Xtensa,
    Riscv32,
    Avr,
    Arm,
}

impl TargetArchitecture {
    pub fn target_triple(&self) -> &'static str {
        match self {
            Self::Xtensa => "xtensa-esp-elf",
            Self::Riscv32 => "riscv32-esp-elf",
            Self::Avr => "avr",
            Self::Arm => "arm-none-eabi",
        }
    }
}

/// Check whether a GCC-specific flag should be removed for clang.
fn should_remove_flag(flag: &str, arch: TargetArchitecture) -> bool {
    // Common GCC-only flags unsupported by clang / IWYU
    match flag {
        "-flto=auto"
        | "-flto"
        | "-fno-fat-lto-objects"
        | "-fuse-linker-plugin"
        | "-ffat-lto-objects"
        | "-freorder-blocks"
        | "-fno-jump-tables" => return true,
        _ => {}
    }

    match arch {
        TargetArchitecture::Xtensa => {
            matches!(
                flag,
                "-mlongcalls"
                    | "-mdisable-hardware-atomics"
                    | "-mfix-esp32-psram-cache-issue"
                    | "-fstrict-volatile-bitfields"
                    | "-mtext-section-literals"
                    | "-fno-tree-switch-conversion"
            ) || flag.starts_with("-mfix-esp32-psram-cache-strategy=")
        }
        TargetArchitecture::Riscv32 => matches!(flag, "-mabi=ilp32" | "-mno-fdiv"),
        TargetArchitecture::Arm => flag == "-mthumb-interwork",
        TargetArchitecture::Avr => false,
    }
}

/// Translate compiler arguments from GCC to clang-compatible equivalents.
///
/// - Replaces the GCC/G++ compiler path with `clang`/`clang++`
/// - Inserts `--target=<triple>` as the second argument
/// - Removes architecture-specific flags that clang doesn't understand
pub fn translate_flags_for_clang(args: &[String], arch: TargetArchitecture) -> Vec<String> {
    if args.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::with_capacity(args.len() + 1);

    // Replace compiler path: detect g++ vs gcc by checking the normalized path
    let compiler_path = args[0].to_lowercase().replace('\\', "/");
    let clang_name = if compiler_path.ends_with("g++") || compiler_path.ends_with("g++.exe") {
        "clang++"
    } else {
        "clang"
    };
    result.push(clang_name.to_string());

    // Add target triple as second argument
    result.push(format!("--target={}", arch.target_triple()));

    // Filter remaining args
    for arg in &args[1..] {
        if !should_remove_flag(arg, arch) {
            result.push(arg.clone());
        }
    }

    result
}

impl CompileDatabase {
    /// Create a new compile database with GCC flags translated to clang equivalents.
    pub fn translate_for_clang(&self, arch: TargetArchitecture) -> CompileDatabase {
        let entries = self
            .entries
            .iter()
            .map(|entry| CompileEntry {
                arguments: translate_flags_for_clang(&entry.arguments, arch),
                directory: entry.directory.clone(),
                file: entry.file.clone(),
                output: entry.output.clone(),
            })
            .collect();
        CompileDatabase { entries }
    }

    /// Prepare compile database for IWYU (include-what-you-use) analysis.
    ///
    /// Transforms the existing (already clang-translated) compile database so that
    /// IWYU can process cross-compiled embedded code:
    ///
    /// - Removes `--target=` flags (IWYU doesn't need code generation support)
    /// - Deduplicates `-D` defines (keeps first occurrence of each key)
    /// - Converts non-project `-I` paths to `-isystem` (suppresses IWYU suggestions)
    /// - Adds extra `-isystem` paths (e.g. GCC toolchain builtin includes)
    pub fn prepare_for_iwyu(
        &self,
        project_src_dir: &Path,
        extra_system_includes: &[PathBuf],
    ) -> CompileDatabase {
        let src_prefix = project_src_dir.to_string_lossy().to_lowercase();
        let entries = self
            .entries
            .iter()
            .map(|entry| {
                let mut args =
                    Vec::with_capacity(entry.arguments.len() + extra_system_includes.len() * 2);
                let mut seen_defines = std::collections::HashSet::new();

                for arg in &entry.arguments {
                    // Remove --target= flags
                    if arg.starts_with("--target=") {
                        continue;
                    }

                    // Deduplicate -D flags (keep first occurrence by key)
                    if arg.starts_with("-D") {
                        let key = if let Some(eq_pos) = arg.find('=') {
                            &arg[..eq_pos]
                        } else {
                            arg.as_str()
                        };
                        if !seen_defines.insert(key.to_string()) {
                            continue;
                        }
                    }

                    // Convert non-project -I to -isystem (suppresses IWYU analysis)
                    if let Some(path) = arg.strip_prefix("-I") {
                        let normalized = path.replace('\\', "/").to_lowercase();
                        if normalized.starts_with(&src_prefix) {
                            args.push(arg.clone());
                        } else {
                            args.push("-isystem".to_string());
                            args.push(path.to_string());
                        }
                        continue;
                    }

                    args.push(arg.clone());
                }

                // Append GCC toolchain builtin include dirs as -isystem
                for inc in extra_system_includes {
                    args.push("-isystem".to_string());
                    args.push(inc.to_string_lossy().to_string());
                }

                CompileEntry {
                    arguments: args,
                    directory: entry.directory.clone(),
                    file: entry.file.clone(),
                    output: entry.output.clone(),
                }
            })
            .collect();
        CompileDatabase { entries }
    }
}

/// Check if a project is a library (has `library.json` at the root).
///
/// Library projects (e.g. FastLED) often have their own build system that
/// generates a correct `compile_commands.json`. We skip overwriting the
/// project root file to avoid clobbering it.
pub fn is_library_project(project_dir: &Path) -> bool {
    project_dir.join("library.json").exists()
}

/// Generate compile database entries for a set of source files.
///
/// # Arguments
/// - `gcc_path` / `gxx_path` — real compiler paths (not cache wrappers)
/// - `c_flags` / `cpp_flags` — language-specific flags
/// - `include_flags` — separate `-I` flags (for ESP32; empty for AVR/Teensy where they're in c/cpp_flags)
/// - `extra_flags` — user/src flags
/// - `sources` — source files to generate entries for
/// - `build_dir` — where object files go (for `-o` path)
/// - `project_dir` — used as the `directory` field
#[allow(clippy::too_many_arguments)]
pub fn generate_entries(
    gcc_path: &Path,
    gxx_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    include_flags: &[String],
    extra_flags: &[String],
    sources: &[PathBuf],
    build_dir: &Path,
    project_dir: &Path,
) -> Vec<CompileEntry> {
    let directory = project_dir.to_string_lossy().to_string();

    sources
        .iter()
        .map(|source| {
            let ext = source
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();

            let (compiler, flags) = match ext.as_str() {
                "c" | "s" => (gcc_path, c_flags),
                _ => (gxx_path, cpp_flags),
            };

            let obj = crate::compiler::CompilerBase::object_path(source, build_dir);

            let mut arguments =
                Vec::with_capacity(1 + flags.len() + include_flags.len() + extra_flags.len() + 4);
            arguments.push(compiler.to_string_lossy().to_string());
            arguments.extend(flags.iter().cloned());
            arguments.extend(include_flags.iter().cloned());
            arguments.extend(extra_flags.iter().cloned());
            arguments.push("-c".to_string());
            arguments.push(source.to_string_lossy().to_string());
            arguments.push("-o".to_string());
            arguments.push(obj.to_string_lossy().to_string());

            CompileEntry {
                arguments,
                directory: directory.clone(),
                file: source.to_string_lossy().to_string(),
                output: Some(obj.to_string_lossy().to_string()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Serialization tests ---

    #[test]
    fn test_compile_entry_serialization() {
        let entry = CompileEntry {
            arguments: vec![
                "/usr/bin/gcc".to_string(),
                "-c".to_string(),
                "main.c".to_string(),
            ],
            directory: "/project".to_string(),
            file: "main.c".to_string(),
            output: Some("main.o".to_string()),
        };

        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["directory"], "/project");
        assert_eq!(json["file"], "main.c");
        assert_eq!(json["output"], "main.o");
        assert!(json["arguments"].is_array());
    }

    #[test]
    fn test_compile_entry_output_none_omitted() {
        let entry = CompileEntry {
            arguments: vec!["/usr/bin/gcc".to_string()],
            directory: "/project".to_string(),
            file: "main.c".to_string(),
            output: None,
        };

        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("output"));
    }

    // --- CompileDatabase container tests ---

    #[test]
    fn test_database_empty() {
        let db = CompileDatabase::new();
        assert!(!db.has_entries());
    }

    #[test]
    fn test_database_add_entry() {
        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec![],
            directory: String::new(),
            file: "test.c".to_string(),
            output: None,
        });
        assert!(db.has_entries());
    }

    #[test]
    fn test_database_write_valid_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec!["/usr/bin/gcc".to_string(), "-c".to_string()],
            directory: "/project".to_string(),
            file: "main.c".to_string(),
            output: None,
        });

        let path = db.write(tmp.path()).unwrap();
        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 1);
        assert_eq!(parsed[0]["file"], "main.c");
    }

    #[test]
    fn test_database_write_creates_parent_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("a").join("b").join("c");
        let db = CompileDatabase::new();
        let path = db.write(&nested).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_database_write_and_copy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        let project_dir = tmp.path().join("project");

        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec![],
            directory: String::new(),
            file: "test.c".to_string(),
            output: None,
        });

        db.write_and_copy(&build_dir, &project_dir).unwrap();
        assert!(build_dir.join("compile_commands.json").exists());
        assert!(project_dir.join("compile_commands.json").exists());
    }

    // --- Cache wrapper stripping tests ---

    #[test]
    fn test_strip_sccache() {
        let args = vec![
            "sccache".to_string(),
            "/usr/bin/gcc".to_string(),
            "-c".to_string(),
        ];
        let stripped = strip_cache_wrapper(&args);
        assert_eq!(stripped[0], "/usr/bin/gcc");
        assert_eq!(stripped.len(), 2);
    }

    #[test]
    fn test_strip_zccache() {
        let args = vec![
            "/path/to/zccache".to_string(),
            "/usr/bin/gcc".to_string(),
            "-c".to_string(),
        ];
        let stripped = strip_cache_wrapper(&args);
        assert_eq!(stripped[0], "/usr/bin/gcc");
    }

    #[test]
    fn test_strip_ccache() {
        let args = vec![
            "ccache".to_string(),
            "/usr/bin/gcc".to_string(),
            "-c".to_string(),
        ];
        let stripped = strip_cache_wrapper(&args);
        assert_eq!(stripped[0], "/usr/bin/gcc");
    }

    #[test]
    fn test_strip_no_wrapper() {
        let args = vec!["/usr/bin/gcc".to_string(), "-c".to_string()];
        let stripped = strip_cache_wrapper(&args);
        assert_eq!(stripped, args);
    }

    #[test]
    fn test_strip_empty() {
        let args: Vec<String> = vec![];
        let stripped = strip_cache_wrapper(&args);
        assert!(stripped.is_empty());
    }

    // --- Entry generation tests ---

    #[test]
    fn test_generate_entries_c_uses_gcc() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &["-std=c11".to_string()],
            &["-std=c++17".to_string()],
            &[],
            &[],
            &[PathBuf::from("main.c")],
            Path::new("/build"),
            Path::new("/project"),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].arguments[0], "/usr/bin/gcc");
        assert!(entries[0].arguments.contains(&"-std=c11".to_string()));
    }

    #[test]
    fn test_generate_entries_cpp_uses_gxx() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &["-std=c11".to_string()],
            &["-std=c++17".to_string()],
            &[],
            &[],
            &[PathBuf::from("main.cpp")],
            Path::new("/build"),
            Path::new("/project"),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
        assert!(entries[0].arguments.contains(&"-std=c++17".to_string()));
    }

    #[test]
    fn test_generate_entries_s_uses_gcc() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &["-std=c11".to_string()],
            &["-std=c++17".to_string()],
            &[],
            &[],
            &[PathBuf::from("startup.s")],
            Path::new("/build"),
            Path::new("/project"),
        );
        assert_eq!(entries[0].arguments[0], "/usr/bin/gcc");
    }

    #[test]
    fn test_generate_entries_empty_sources() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &[],
            &[],
            Path::new("/build"),
            Path::new("/project"),
        );
        assert!(entries.is_empty());
    }

    #[test]
    fn test_generate_entries_include_flags_in_args() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &["-I/sdk/include".to_string(), "-I/core/include".to_string()],
            &[],
            &[PathBuf::from("main.cpp")],
            Path::new("/build"),
            Path::new("/project"),
        );
        assert!(entries[0].arguments.contains(&"-I/sdk/include".to_string()));
        assert!(entries[0]
            .arguments
            .contains(&"-I/core/include".to_string()));
    }

    #[test]
    fn test_generate_entries_extra_flags_in_args() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &["-DUSER_FLAG=1".to_string()],
            &[PathBuf::from("main.cpp")],
            Path::new("/build"),
            Path::new("/project"),
        );
        assert!(entries[0].arguments.contains(&"-DUSER_FLAG=1".to_string()));
    }

    #[test]
    fn test_generate_entries_directory_is_project_dir() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &[],
            &[PathBuf::from("main.cpp")],
            Path::new("/build"),
            Path::new("/my/project"),
        );
        assert_eq!(entries[0].directory, "/my/project");
    }

    #[test]
    fn test_generate_entries_arguments_structure() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &["-Os".to_string()],
            &["-Os".to_string()],
            &["-I/inc".to_string()],
            &["-DFOO".to_string()],
            &[PathBuf::from("main.c")],
            Path::new("/build"),
            Path::new("/project"),
        );

        let args = &entries[0].arguments;
        // Starts with compiler
        assert_eq!(args[0], "/usr/bin/gcc");
        // Ends with -c source -o object
        let len = args.len();
        assert_eq!(args[len - 4], "-c");
        assert_eq!(args[len - 3], "main.c");
        assert_eq!(args[len - 2], "-o");
    }

    // =========================================================================
    // Adversarial tests — designed to expose edge-case bugs
    // =========================================================================

    // --- Cache wrapper stripping edge cases ---

    #[test]
    fn test_strip_cache_wrapper_windows_exe() {
        let args = vec![
            "C:\\Users\\user\\.cargo\\bin\\sccache.exe".to_string(),
            "C:\\tools\\gcc.exe".to_string(),
            "-c".to_string(),
        ];
        let stripped = strip_cache_wrapper(&args);
        assert_eq!(stripped[0], "C:\\tools\\gcc.exe");
        assert_eq!(stripped.len(), 2);
    }

    #[test]
    fn test_strip_cache_wrapper_case_insensitive() {
        // Windows file systems are case-insensitive
        for name in &[
            "SCCACHE", "Sccache", "ZCCACHE", "Zccache", "CCACHE", "Ccache",
        ] {
            let args = vec![
                name.to_string(),
                "/usr/bin/gcc".to_string(),
                "-c".to_string(),
            ];
            let stripped = strip_cache_wrapper(&args);
            assert_eq!(
                stripped[0], "/usr/bin/gcc",
                "failed to strip cache wrapper: {}",
                name
            );
        }
    }

    #[test]
    fn test_strip_cache_wrapper_single_element_wrapper() {
        // Only the wrapper, no actual compiler — should return as-is
        let args = vec!["sccache".to_string()];
        let stripped = strip_cache_wrapper(&args);
        assert_eq!(stripped, args);
    }

    #[test]
    fn test_strip_cache_wrapper_not_a_wrapper() {
        // File named "sccache-stats" shouldn't be stripped
        let args = vec!["sccache-stats".to_string(), "/usr/bin/gcc".to_string()];
        let stripped = strip_cache_wrapper(&args);
        // file_stem of "sccache-stats" is "sccache-stats", not "sccache"
        assert_eq!(stripped.len(), 2);
        assert_eq!(stripped[0], "sccache-stats");
    }

    // --- Extension classification adversarial tests ---

    #[test]
    fn test_generate_entries_uppercase_c_extension() {
        // .C is treated as C++ on some systems, but our lowercase normalization
        // maps it to "c" → gcc. This matches GCC behavior.
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &[],
            &[PathBuf::from("main.C")],
            Path::new("/build"),
            Path::new("/project"),
        );
        // After to_lowercase(), ".C" becomes "c" → uses gcc
        assert_eq!(entries[0].arguments[0], "/usr/bin/gcc");
    }

    #[test]
    fn test_generate_entries_cc_extension_uses_gxx() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &[],
            &[PathBuf::from("module.cc")],
            Path::new("/build"),
            Path::new("/project"),
        );
        assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
    }

    #[test]
    fn test_generate_entries_cxx_extension_uses_gxx() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &[],
            &[PathBuf::from("module.cxx")],
            Path::new("/build"),
            Path::new("/project"),
        );
        assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
    }

    #[test]
    fn test_generate_entries_ino_cpp_uses_gxx() {
        // Preprocessed .ino files become .ino.cpp — extension is "cpp"
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &[],
            &[PathBuf::from("sketch.ino.cpp")],
            Path::new("/build"),
            Path::new("/project"),
        );
        assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
    }

    #[test]
    fn test_generate_entries_no_extension_uses_gxx() {
        // Files without extension fall through to g++ (the default branch)
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &[],
            &[PathBuf::from("Makefile")],
            Path::new("/build"),
            Path::new("/project"),
        );
        assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
    }

    #[test]
    fn test_generate_entries_uppercase_s_assembly_uses_gcc() {
        // .S (uppercase) is GCC-preprocessed assembly, should use gcc
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &[],
            &[PathBuf::from("boot.S")],
            Path::new("/build"),
            Path::new("/project"),
        );
        // to_lowercase() → "s" → matches gcc branch
        assert_eq!(entries[0].arguments[0], "/usr/bin/gcc");
    }

    // --- Path handling adversarial tests ---

    #[test]
    fn test_generate_entries_paths_with_spaces() {
        let entries = generate_entries(
            Path::new("/usr/bin/my gcc"),
            Path::new("/usr/bin/my g++"),
            &[],
            &[],
            &["-I/path with spaces/include".to_string()],
            &[],
            &[PathBuf::from("/my project/src/main.cpp")],
            Path::new("/my build"),
            Path::new("/my project"),
        );
        assert_eq!(entries[0].directory, "/my project");
        assert_eq!(entries[0].file, "/my project/src/main.cpp");
        assert!(entries[0]
            .arguments
            .contains(&"-I/path with spaces/include".to_string()));
    }

    #[test]
    fn test_generate_entries_windows_backslash_paths() {
        let entries = generate_entries(
            Path::new("C:\\tools\\gcc.exe"),
            Path::new("C:\\tools\\g++.exe"),
            &[],
            &[],
            &[],
            &[],
            &[PathBuf::from("C:\\Users\\user\\project\\src\\main.cpp")],
            Path::new("C:\\Users\\user\\build"),
            Path::new("C:\\Users\\user\\project"),
        );
        // to_string_lossy preserves original path separators
        assert!(!entries[0].file.is_empty());
        assert!(!entries[0].directory.is_empty());
        // The output field should point to the build dir
        assert!(
            entries[0].output.as_ref().unwrap().contains("build")
                || entries[0].output.as_ref().unwrap().contains("Users")
        );
    }

    // --- Arguments must never contain @file (response file) references ---

    #[test]
    fn test_generate_entries_no_response_file_in_args() {
        // Even with many include flags, generate_entries should produce
        // individual -I flags, never @file references (those are for GCC only).
        let include_flags: Vec<String> =
            (0..300).map(|i| format!("-I/sdk/include/{}", i)).collect();

        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &include_flags,
            &[],
            &[PathBuf::from("main.cpp")],
            Path::new("/build"),
            Path::new("/project"),
        );

        for arg in &entries[0].arguments {
            assert!(
                !arg.starts_with('@'),
                "compile_commands.json must not contain @file references: {}",
                arg
            );
        }
        // All 300 include flags should be present individually
        assert!(entries[0]
            .arguments
            .contains(&"-I/sdk/include/0".to_string()));
        assert!(entries[0]
            .arguments
            .contains(&"-I/sdk/include/299".to_string()));
    }

    // --- File field must be the source path, not the build path ---

    #[test]
    fn test_generate_entries_file_is_source_not_build() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &[],
            &[PathBuf::from("/project/src/main.cpp")],
            Path::new("/project/.fbuild/build/esp32/src"),
            Path::new("/project"),
        );
        assert_eq!(entries[0].file, "/project/src/main.cpp");
        // Output should be in the build dir
        assert!(
            entries[0].output.as_ref().unwrap().contains(".fbuild"),
            "output should be in build dir: {:?}",
            entries[0].output
        );
    }

    // --- write_and_copy: both files must have identical content ---

    #[test]
    fn test_write_and_copy_identical_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        let project_dir = tmp.path().join("project");

        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec![
                "/usr/bin/g++".to_string(),
                "-c".to_string(),
                "main.cpp".to_string(),
            ],
            directory: "/project".to_string(),
            file: "main.cpp".to_string(),
            output: Some("main.cpp.o".to_string()),
        });

        db.write_and_copy(&build_dir, &project_dir).unwrap();

        let build_content =
            std::fs::read_to_string(build_dir.join("compile_commands.json")).unwrap();
        let project_content =
            std::fs::read_to_string(project_dir.join("compile_commands.json")).unwrap();
        assert_eq!(build_content, project_content);
    }

    // --- write_and_copy: suppressed when library.json exists ---

    #[test]
    fn test_write_and_copy_suppressed_for_library_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();

        // Create library.json to simulate a library project (like FastLED)
        std::fs::write(
            project_dir.join("library.json"),
            r#"{"name": "FastLED", "version": "3.10.3"}"#,
        )
        .unwrap();

        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec!["/usr/bin/g++".to_string()],
            directory: project_dir.to_string_lossy().to_string(),
            file: "main.cpp".to_string(),
            output: None,
        });

        let result_path = db.write_and_copy(&build_dir, &project_dir).unwrap();

        // Build dir should have the file
        assert!(build_dir.join("compile_commands.json").exists());
        // Project dir should NOT have compile_commands.json (suppressed)
        assert!(
            !project_dir.join("compile_commands.json").exists(),
            "compile_commands.json should NOT be copied to project root for library projects"
        );
        // The returned path should be the build dir path
        assert_eq!(result_path, build_dir.join("compile_commands.json"));
    }

    #[test]
    fn test_write_and_copy_not_suppressed_for_sketch_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();

        // No library.json — this is a normal sketch project
        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec!["/usr/bin/g++".to_string()],
            directory: project_dir.to_string_lossy().to_string(),
            file: "main.cpp".to_string(),
            output: None,
        });

        db.write_and_copy(&build_dir, &project_dir).unwrap();

        // Both should exist
        assert!(build_dir.join("compile_commands.json").exists());
        assert!(project_dir.join("compile_commands.json").exists());
    }

    // --- is_library_project detection ---

    #[test]
    fn test_is_library_project_with_library_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("library.json"), r#"{"name": "MyLib"}"#).unwrap();
        assert!(is_library_project(tmp.path()));
    }

    #[test]
    fn test_is_library_project_without_library_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(!is_library_project(tmp.path()));
    }

    // --- Empty database produces valid JSON ---

    #[test]
    fn test_write_empty_database_valid_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = CompileDatabase::new();
        let path = db.write(tmp.path()).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed.is_array());
        assert!(parsed.as_array().unwrap().is_empty());
    }

    // --- Mixed source types in a single call ---

    #[test]
    fn test_generate_entries_mixed_sources() {
        let sources = vec![
            PathBuf::from("main.cpp"),
            PathBuf::from("util.c"),
            PathBuf::from("boot.S"),
            PathBuf::from("driver.cc"),
            PathBuf::from("algo.cxx"),
        ];
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &["-std=c11".to_string()],
            &["-std=c++17".to_string()],
            &[],
            &[],
            &sources,
            Path::new("/build"),
            Path::new("/project"),
        );
        assert_eq!(entries.len(), 5);

        // main.cpp → g++
        assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
        assert!(entries[0].arguments.contains(&"-std=c++17".to_string()));

        // util.c → gcc
        assert_eq!(entries[1].arguments[0], "/usr/bin/gcc");
        assert!(entries[1].arguments.contains(&"-std=c11".to_string()));

        // boot.S → gcc (assembly)
        assert_eq!(entries[2].arguments[0], "/usr/bin/gcc");

        // driver.cc → g++
        assert_eq!(entries[3].arguments[0], "/usr/bin/g++");

        // algo.cxx → g++
        assert_eq!(entries[4].arguments[0], "/usr/bin/g++");
    }

    // --- Duplicate sources don't panic ---

    #[test]
    fn test_generate_entries_duplicate_sources() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &[],
            &[PathBuf::from("main.cpp"), PathBuf::from("main.cpp")],
            Path::new("/build"),
            Path::new("/project"),
        );
        // Both entries should exist (dedup is the caller's responsibility)
        assert_eq!(entries.len(), 2);
    }

    // --- Include flags with build dir paths (the clangd navigation issue) ---

    #[test]
    fn test_generate_entries_include_flags_preserved_verbatim() {
        // The compile database should faithfully reproduce whatever include
        // flags it receives. The ORCHESTRATOR is responsible for passing
        // source-tree paths, not build-dir paths.
        let include_flags = vec![
            "-I/project/src".to_string(), // source tree ✓
            "-I/home/user/.fbuild/build/esp32/libs/fastled/src".to_string(), // cache path ✗
            "-I/framework/cores/esp32".to_string(), // framework ✓
        ];
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &include_flags,
            &[],
            &[PathBuf::from("main.cpp")],
            Path::new("/build"),
            Path::new("/project"),
        );
        // All include flags should be present, unmodified
        for flag in &include_flags {
            assert!(
                entries[0].arguments.contains(flag),
                "missing include flag: {}",
                flag
            );
        }
    }

    // --- Output path uses build_dir, not project source dir ---

    #[test]
    fn test_generate_entries_output_in_build_dir() {
        let entries = generate_entries(
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            &[],
            &[],
            &[],
            &[],
            &[PathBuf::from("/project/src/main.cpp")],
            Path::new("/build/obj"),
            Path::new("/project"),
        );
        let output = entries[0].output.as_ref().unwrap();
        assert!(
            output.starts_with("/build/obj"),
            "output should start with build dir: {}",
            output
        );
    }

    // --- Extend adds all entries ---

    #[test]
    fn test_database_extend_accumulates() {
        let mut db = CompileDatabase::new();
        let entries1 = vec![CompileEntry {
            arguments: vec![],
            directory: String::new(),
            file: "a.c".to_string(),
            output: None,
        }];
        let entries2 = vec![
            CompileEntry {
                arguments: vec![],
                directory: String::new(),
                file: "b.c".to_string(),
                output: None,
            },
            CompileEntry {
                arguments: vec![],
                directory: String::new(),
                file: "c.c".to_string(),
                output: None,
            },
        ];
        db.extend(entries1);
        db.extend(entries2);
        // Should have all 3 entries
        let tmp = tempfile::TempDir::new().unwrap();
        let path = db.write(tmp.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 3);
    }

    // =========================================================================
    // Clang flag translation tests
    // =========================================================================

    #[test]
    fn test_target_triples() {
        assert_eq!(TargetArchitecture::Xtensa.target_triple(), "xtensa-esp-elf");
        assert_eq!(
            TargetArchitecture::Riscv32.target_triple(),
            "riscv32-esp-elf"
        );
        assert_eq!(TargetArchitecture::Avr.target_triple(), "avr");
        assert_eq!(TargetArchitecture::Arm.target_triple(), "arm-none-eabi");
    }

    #[test]
    fn test_translate_gcc_to_clang() {
        let args = vec![
            "/usr/bin/avr-gcc".to_string(),
            "-Os".to_string(),
            "-c".to_string(),
        ];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Avr);
        assert_eq!(result[0], "clang");
    }

    #[test]
    fn test_translate_gxx_to_clangxx() {
        let args = vec!["/usr/bin/arm-none-eabi-g++".to_string(), "-Os".to_string()];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Arm);
        assert_eq!(result[0], "clang++");
    }

    #[test]
    fn test_translate_windows_compiler_path() {
        let args = vec![
            "C:\\tools\\xtensa-esp32-elf-g++.exe".to_string(),
            "-Os".to_string(),
        ];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Xtensa);
        assert_eq!(result[0], "clang++");
    }

    #[test]
    fn test_translate_adds_target() {
        let args = vec!["/usr/bin/gcc".to_string(), "-c".to_string()];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Xtensa);
        assert_eq!(result[1], "--target=xtensa-esp-elf");
    }

    #[test]
    fn test_translate_removes_common_lto_flags() {
        let args = vec![
            "/usr/bin/gcc".to_string(),
            "-flto=auto".to_string(),
            "-flto".to_string(),
            "-fno-fat-lto-objects".to_string(),
            "-fuse-linker-plugin".to_string(),
            "-ffat-lto-objects".to_string(),
            "-Os".to_string(),
        ];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Avr);
        assert!(!result.contains(&"-flto=auto".to_string()));
        assert!(!result.contains(&"-flto".to_string()));
        assert!(!result.contains(&"-fno-fat-lto-objects".to_string()));
        assert!(!result.contains(&"-fuse-linker-plugin".to_string()));
        assert!(!result.contains(&"-ffat-lto-objects".to_string()));
        assert!(result.contains(&"-Os".to_string()));
    }

    #[test]
    fn test_translate_xtensa_removals() {
        let args = vec![
            "/usr/bin/xtensa-esp32-elf-gcc".to_string(),
            "-mlongcalls".to_string(),
            "-mdisable-hardware-atomics".to_string(),
            "-mfix-esp32-psram-cache-issue".to_string(),
            "-fstrict-volatile-bitfields".to_string(),
            "-mtext-section-literals".to_string(),
            "-fno-tree-switch-conversion".to_string(),
            "-Os".to_string(),
        ];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Xtensa);
        assert!(!result.contains(&"-mlongcalls".to_string()));
        assert!(!result.contains(&"-mdisable-hardware-atomics".to_string()));
        assert!(!result.contains(&"-mfix-esp32-psram-cache-issue".to_string()));
        assert!(!result.contains(&"-fstrict-volatile-bitfields".to_string()));
        assert!(!result.contains(&"-mtext-section-literals".to_string()));
        assert!(!result.contains(&"-fno-tree-switch-conversion".to_string()));
        assert!(result.contains(&"-Os".to_string()));
    }

    #[test]
    fn test_translate_xtensa_psram_strategy_prefix() {
        let args = vec![
            "/usr/bin/gcc".to_string(),
            "-mfix-esp32-psram-cache-strategy=memw".to_string(),
            "-Os".to_string(),
        ];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Xtensa);
        assert!(!result.contains(&"-mfix-esp32-psram-cache-strategy=memw".to_string()));
        assert!(result.contains(&"-Os".to_string()));
    }

    #[test]
    fn test_translate_riscv_removals() {
        let args = vec![
            "/usr/bin/riscv32-esp-elf-gcc".to_string(),
            "-mabi=ilp32".to_string(),
            "-mno-fdiv".to_string(),
            "-march=rv32imac".to_string(),
            "-Os".to_string(),
        ];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Riscv32);
        assert!(!result.contains(&"-mabi=ilp32".to_string()));
        assert!(!result.contains(&"-mno-fdiv".to_string()));
        assert!(result.contains(&"-march=rv32imac".to_string()));
    }

    #[test]
    fn test_translate_arm_removals() {
        let args = vec![
            "/usr/bin/arm-none-eabi-g++".to_string(),
            "-mthumb-interwork".to_string(),
            "-mcpu=cortex-m7".to_string(),
            "-Os".to_string(),
        ];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Arm);
        assert!(!result.contains(&"-mthumb-interwork".to_string()));
        assert!(result.contains(&"-mcpu=cortex-m7".to_string()));
    }

    #[test]
    fn test_translate_avr_no_extra_removals() {
        let args = vec![
            "/usr/bin/avr-gcc".to_string(),
            "-mmcu=atmega328p".to_string(),
            "-Os".to_string(),
        ];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Avr);
        assert!(result.contains(&"-mmcu=atmega328p".to_string()));
        assert!(result.contains(&"-Os".to_string()));
    }

    #[test]
    fn test_translate_preserves_includes_and_defines() {
        let args = vec![
            "/usr/bin/gcc".to_string(),
            "-I/path/to/include".to_string(),
            "-DFOO=1".to_string(),
            "-c".to_string(),
        ];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Avr);
        assert!(result.contains(&"-I/path/to/include".to_string()));
        assert!(result.contains(&"-DFOO=1".to_string()));
    }

    #[test]
    fn test_translate_empty_args() {
        let args: Vec<String> = vec![];
        let result = translate_flags_for_clang(&args, TargetArchitecture::Avr);
        assert!(result.is_empty());
    }

    #[test]
    fn test_database_translate_for_clang() {
        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec![
                "/usr/bin/xtensa-esp32-elf-gcc".to_string(),
                "-mlongcalls".to_string(),
                "-Os".to_string(),
                "-c".to_string(),
                "main.c".to_string(),
            ],
            directory: "/project".to_string(),
            file: "main.c".to_string(),
            output: Some("main.o".to_string()),
        });
        db.add_entry(CompileEntry {
            arguments: vec![
                "/usr/bin/xtensa-esp32-elf-g++".to_string(),
                "-mlongcalls".to_string(),
                "-std=c++17".to_string(),
                "-c".to_string(),
                "app.cpp".to_string(),
            ],
            directory: "/project".to_string(),
            file: "app.cpp".to_string(),
            output: Some("app.o".to_string()),
        });

        let translated = db.translate_for_clang(TargetArchitecture::Xtensa);

        // First entry: gcc → clang
        assert_eq!(translated.entries[0].arguments[0], "clang");
        assert_eq!(
            translated.entries[0].arguments[1],
            "--target=xtensa-esp-elf"
        );
        assert!(!translated.entries[0]
            .arguments
            .contains(&"-mlongcalls".to_string()));
        assert!(translated.entries[0].arguments.contains(&"-Os".to_string()));
        assert_eq!(translated.entries[0].file, "main.c");

        // Second entry: g++ → clang++
        assert_eq!(translated.entries[1].arguments[0], "clang++");
        assert!(!translated.entries[1]
            .arguments
            .contains(&"-mlongcalls".to_string()));
        assert!(translated.entries[1]
            .arguments
            .contains(&"-std=c++17".to_string()));
    }

    #[test]
    fn test_translate_does_not_modify_original() {
        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec![
                "/usr/bin/gcc".to_string(),
                "-mlongcalls".to_string(),
                "-Os".to_string(),
            ],
            directory: "/project".to_string(),
            file: "main.c".to_string(),
            output: None,
        });

        let _translated = db.translate_for_clang(TargetArchitecture::Xtensa);

        // Original should still have -mlongcalls
        assert!(db.entries[0].arguments.contains(&"-mlongcalls".to_string()));
        assert_eq!(db.entries[0].arguments[0], "/usr/bin/gcc");
    }

    // =========================================================================
    // IWYU preparation tests
    // =========================================================================

    #[test]
    fn test_should_remove_freorder_blocks() {
        assert!(should_remove_flag(
            "-freorder-blocks",
            TargetArchitecture::Xtensa
        ));
        assert!(should_remove_flag(
            "-freorder-blocks",
            TargetArchitecture::Avr
        ));
    }

    #[test]
    fn test_should_remove_fno_jump_tables() {
        assert!(should_remove_flag(
            "-fno-jump-tables",
            TargetArchitecture::Xtensa
        ));
    }

    #[test]
    fn test_fstack_protector_preserved() {
        // -fstack-protector is supported by clang — keep it
        assert!(!should_remove_flag(
            "-fstack-protector",
            TargetArchitecture::Xtensa
        ));
    }

    #[test]
    fn test_prepare_for_iwyu_removes_target() {
        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec![
                "clang++".into(),
                "--target=xtensa-esp-elf".into(),
                "-Os".into(),
                "-c".into(),
                "src/main.cpp".into(),
            ],
            directory: "/project".into(),
            file: "src/main.cpp".into(),
            output: None,
        });
        let result = db.prepare_for_iwyu(Path::new("/project/src"), &[]);
        assert!(!result.entries[0]
            .arguments
            .iter()
            .any(|a| a.starts_with("--target=")));
    }

    #[test]
    fn test_prepare_for_iwyu_dedup_defines() {
        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec![
                "clang".into(),
                "-DFOO=1".into(),
                "-DBAR".into(),
                "-DFOO=2".into(), // duplicate, should be dropped
            ],
            directory: "/project".into(),
            file: "src/main.c".into(),
            output: None,
        });
        let result = db.prepare_for_iwyu(Path::new("/project/src"), &[]);
        let defines: Vec<&str> = result.entries[0]
            .arguments
            .iter()
            .filter(|a| a.starts_with("-D"))
            .map(|a| a.as_str())
            .collect();
        assert_eq!(defines, vec!["-DFOO=1", "-DBAR"]);
    }

    #[test]
    fn test_prepare_for_iwyu_converts_system_includes() {
        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec![
                "clang".into(),
                "-I/project/src/mylib".into(),
                "-I/usr/include/esp32".into(),
            ],
            directory: "/project".into(),
            file: "src/main.c".into(),
            output: None,
        });
        let result = db.prepare_for_iwyu(Path::new("/project/src"), &[]);
        let args = &result.entries[0].arguments;
        // Project include kept as -I
        assert!(args.contains(&"-I/project/src/mylib".to_string()));
        // System include converted to -isystem
        assert!(args.contains(&"-isystem".to_string()));
        assert!(args.contains(&"/usr/include/esp32".to_string()));
        assert!(!args.contains(&"-I/usr/include/esp32".to_string()));
    }

    #[test]
    fn test_prepare_for_iwyu_adds_extra_system_includes() {
        let mut db = CompileDatabase::new();
        db.add_entry(CompileEntry {
            arguments: vec!["clang".into(), "-c".into(), "src/main.c".into()],
            directory: "/project".into(),
            file: "src/main.c".into(),
            output: None,
        });
        let extras = vec![PathBuf::from("/toolchain/lib/gcc/xtensa/14/include")];
        let result = db.prepare_for_iwyu(Path::new("/project/src"), &extras);
        let args = &result.entries[0].arguments;
        assert!(args.contains(&"-isystem".to_string()));
        assert!(args.contains(&"/toolchain/lib/gcc/xtensa/14/include".to_string()));
    }
}
