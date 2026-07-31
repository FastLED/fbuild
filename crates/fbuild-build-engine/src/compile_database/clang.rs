//! Clang flag translation and IWYU preparation.

use std::path::{Path, PathBuf};

use super::types::{CompileDatabase, CompileEntry, TargetArchitecture};

/// Check whether a GCC-specific flag should be removed for clang.
pub(super) fn should_remove_flag(flag: &str, arch: TargetArchitecture) -> bool {
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
    // FastLED/fbuild#911 — path-shape slash normalization goes through
    // `NormalizedPath::display_slash()`.
    let compiler_path = fbuild_core::path::NormalizedPath::from(args[0].as_str())
        .display_slash()
        .to_lowercase();
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

/// Deterministic, deduplicated `-isystem <dir>` args for the GCC toolchain's
/// builtin include directories (`stdbool.h`, `stddef.h`, `stdarg.h`, etc. —
/// implicit GCC search paths that never appear in `compile_commands.json`
/// because GCC adds them automatically).
///
/// clangd has no such implicit search path, so without these baked in as
/// `-isystem` it can't find those headers. Previously the only fix was a
/// `--query-driver` clangd argument asking clangd to *run* the real compiler
/// and ask it — but `translate_for_clang` (this same function) already
/// rewrites `arguments[0]` to bare `clang`/`clang++`, which made
/// `--query-driver` resolve to nothing useful (FastLED/fbuild#1076 Phase 0).
/// Baking the dirs in directly is robust on every platform and doesn't
/// require clangd to shell out to anything.
///
/// Empty (no-op) when no toolchain is cached yet — never fails a build.
fn builtin_isystem_args() -> Vec<String> {
    isystem_args_from_dirs(fbuild_packages::toolchain::clang::find_gcc_builtin_include_dirs())
}

/// Sort + dedup a list of include dirs and flatten it into `-isystem <dir>`
/// pairs. Pulled out of [`builtin_isystem_args`] so the sort/dedup/flatten
/// logic is unit-testable without depending on (or mutating) the real
/// toolchain cache directory.
pub(super) fn isystem_args_from_dirs(mut dirs: Vec<PathBuf>) -> Vec<String> {
    dirs.sort();
    dirs.dedup();

    let mut args = Vec::with_capacity(dirs.len() * 2);
    for dir in dirs {
        args.push("-isystem".to_string());
        args.push(dir.to_string_lossy().to_string());
    }
    args
}

impl CompileDatabase {
    /// Create a new compile database with GCC flags translated to clang
    /// equivalents, with the toolchain's GCC builtin include dirs baked in
    /// as `-isystem` (see `builtin_isystem_args`, private to this module).
    pub fn translate_for_clang(&self, arch: TargetArchitecture) -> CompileDatabase {
        let builtin_includes = builtin_isystem_args();
        let entries = self
            .entries
            .iter()
            .map(|entry| {
                let mut arguments = translate_flags_for_clang(&entry.arguments, arch);
                arguments.extend(builtin_includes.iter().cloned());
                CompileEntry {
                    arguments,
                    directory: entry.directory.clone(),
                    file: entry.file.clone(),
                    output: entry.output.clone(),
                }
            })
            .collect();
        CompileDatabase { entries }
    }

    /// Swap the generated `<stem>.ino.cpp` compile entry for one raw-`.ino`
    /// entry per tab (FastLED/fbuild#1076 Phase 0 — IDE-flavored compile DB;
    /// direction update: "use the converter's `.ino.cpp` output for clangd
    /// IntelliSense").
    ///
    /// clangd analyzes the live text of whatever file is open in the editor.
    /// An entry naming the generated `.ino.cpp` means unsaved edits to the
    /// sketch are invisible and diagnostics point at a file nobody is
    /// looking at. Each raw `.ino` instead gets the generated entry's
    /// (already clang-translated) flags plus `-x c++ -include <prelude>`,
    /// with the `file` field and the `-c <file>` / `file` argument swapped to
    /// the raw `.ino` path. The generated `.ino.cpp` entry is removed —
    /// keeping both would double-index the same code under two translation
    /// units and confuse go-to-definition.
    ///
    /// No-op (including: does not remove the generated entry) when
    /// `ino_preludes` is empty — i.e. no `.ino` tabs were preprocessed for
    /// this build (no `.ino` files, or `main.cpp` skipped preprocessing).
    pub fn swap_ino_entries_for_raw(&self, ino_preludes: &[(PathBuf, PathBuf)]) -> CompileDatabase {
        if ino_preludes.is_empty() {
            return CompileDatabase {
                entries: self.entries.clone(),
            };
        }

        let mut template: Option<&CompileEntry> = None;
        let mut entries: Vec<CompileEntry> =
            Vec::with_capacity(self.entries.len() + ino_preludes.len());
        for entry in &self.entries {
            if entry.file.ends_with(".ino.cpp") {
                template = Some(entry);
                continue;
            }
            entries.push(entry.clone());
        }

        let Some(template) = template else {
            // No generated .ino.cpp entry present — leave the database as-is
            // rather than silently dropping something unexpected.
            return CompileDatabase {
                entries: self.entries.clone(),
            };
        };

        for (raw_ino, prelude) in ino_preludes {
            entries.push(raw_ino_entry_from_template(template, raw_ino, prelude));
        }

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
                        // FastLED/fbuild#911 — path-shape slash normalization
                        // goes through `NormalizedPath::display_slash()`.
                        let normalized = fbuild_core::path::NormalizedPath::from(path)
                            .display_slash()
                            .to_lowercase();
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

/// Build one raw-`.ino` compile entry from the generated `.ino.cpp` entry's
/// (already clang-translated) flags: insert `-x c++ -include <prelude>`
/// right before `-c`, and swap every argument that names the generated file
/// (the `-c <file>` argument) for the raw `.ino` path.
fn raw_ino_entry_from_template(
    template: &CompileEntry,
    raw_ino: &Path,
    prelude: &Path,
) -> CompileEntry {
    let raw_ino_str = raw_ino.to_string_lossy().to_string();
    let prelude_str = prelude.to_string_lossy().to_string();

    let mut arguments = Vec::with_capacity(template.arguments.len() + 4);
    let mut inserted_flavor = false;
    for arg in &template.arguments {
        if !inserted_flavor && arg == "-c" {
            arguments.push("-x".to_string());
            arguments.push("c++".to_string());
            arguments.push("-include".to_string());
            arguments.push(prelude_str.clone());
            inserted_flavor = true;
        }

        if *arg == template.file {
            arguments.push(raw_ino_str.clone());
        } else {
            arguments.push(arg.clone());
        }
    }

    CompileEntry {
        arguments,
        directory: template.directory.clone(),
        file: raw_ino_str,
        output: template.output.clone(),
    }
}
