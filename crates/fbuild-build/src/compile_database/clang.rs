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
