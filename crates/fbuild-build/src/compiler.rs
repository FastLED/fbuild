//! Compiler traits and base implementation.
//!
//! Defines the `Compiler` trait and `CompilerBase` shared logic for
//! building compiler flags, invoking gcc/g++, and detecting rebuilds.

use fbuild_core::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

// ── Shared config types (used by all platform MCU configs) ──────────────

/// Compiler flags split by language.
#[derive(Debug, Clone, Deserialize)]
pub struct CompilerFlags {
    pub common: Vec<String>,
    pub c: Vec<String>,
    pub cxx: Vec<String>,
}

/// Profile-specific build flags (release, quick).
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileFlags {
    pub compile_flags: Vec<String>,
    pub link_flags: Vec<String>,
}

/// Objcopy configuration for firmware conversion (AVR and Teensy).
#[derive(Debug, Clone, Deserialize)]
pub struct ObjcopyConfig {
    pub output_format: String,
    pub remove_sections: Vec<String>,
}

/// Common interface for platform MCU configurations.
///
/// Provides the minimal surface needed by shared compiler helpers.
/// Platform-specific details (esptool config, compat_defines, etc.) remain
/// on the concrete types.
pub trait McuConfig {
    /// Get the compiler flags (common, C, C++).
    fn compiler_flags(&self) -> &CompilerFlags;

    /// Get profile-specific flags by name (e.g., "release", "quick").
    fn get_profile(&self, name: &str) -> Option<&ProfileFlags>;
}

/// Result of compiling a single source file.
#[derive(Debug)]
pub struct CompileResult {
    pub success: bool,
    pub object_file: PathBuf,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Trait for platform-specific compilers.
pub trait Compiler: Send + Sync {
    /// Platform-specific compilation dispatch.
    ///
    /// Routes to `compile_source()` with platform-specific parameters
    /// (temp dir, response file prefix, compiler cache, extra pre-flags).
    fn compile_one(
        &self,
        compiler_path: &Path,
        source: &Path,
        output: &Path,
        flags: &[String],
        extra_flags: &[String],
    ) -> Result<CompileResult>;

    /// Compile a C source file to an object file.
    fn compile_c(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        let flags = self.c_flags();
        self.compile_one(self.gcc_path(), source, output, &flags, extra_flags)
    }

    /// Compile a C++ source file to an object file.
    fn compile_cpp(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        let flags = self.cpp_flags();
        self.compile_one(self.gxx_path(), source, output, &flags, extra_flags)
    }

    /// Compile a source file (auto-detect C vs C++).
    fn compile(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        let ext = source
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        match ext.as_str() {
            "c" | "s" => self.compile_c(source, output, extra_flags),
            _ => self.compile_cpp(source, output, extra_flags),
        }
    }

    /// Path to the C compiler (gcc).
    fn gcc_path(&self) -> &Path;

    /// Path to the C++ compiler (g++).
    fn gxx_path(&self) -> &Path;

    /// C compiler flags (without extra_flags).
    fn c_flags(&self) -> Vec<String>;

    /// C++ compiler flags (without extra_flags).
    fn cpp_flags(&self) -> Vec<String>;
}

/// Shared compiler utilities used by all platform-specific compilers.
pub struct CompilerBase {
    pub mcu: String,
    pub f_cpu: String,
    pub defines: HashMap<String, String>,
    pub include_dirs: Vec<PathBuf>,
    pub verbose: bool,
}

impl CompilerBase {
    /// Build `-D` flags from the defines map.
    ///
    /// Flags are sorted by key to ensure deterministic ordering across builds.
    /// This is critical for zccache: non-deterministic flag order causes different
    /// command hashes → 0% cache hit rate.
    pub fn build_define_flags(&self) -> Vec<String> {
        let mut flags: Vec<String> = self
            .defines
            .iter()
            .map(|(k, v)| {
                if v == "1" {
                    format!("-D{}", k)
                } else {
                    format!("-D{}={}", k, v)
                }
            })
            .collect();
        flags.sort();
        flags
    }

    /// Build `-I` flags from include directories.
    pub fn build_include_flags(&self) -> Vec<String> {
        self.include_dirs
            .iter()
            .map(|d| format!("-I{}", d.display()))
            .collect()
    }

    /// Check if a source file needs rebuilding (source newer than object).
    pub fn needs_rebuild(source: &Path, object: &Path) -> bool {
        if !object.exists() {
            return true;
        }

        let src_time = source
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let obj_time = object
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        src_time > obj_time
    }

    /// Compute the output .o path for a source file.
    pub fn object_path(source: &Path, build_dir: &Path) -> PathBuf {
        let stem = source.file_stem().unwrap_or_default().to_string_lossy();
        let ext = source.extension().unwrap_or_default().to_string_lossy();
        build_dir.join(format!("{}.{}.o", stem, ext))
    }
}

/// Get the platform-appropriate temp directory for response files.
///
/// Delegates to [`fbuild_core::response_file::windows_temp_dir`].
pub fn windows_temp_dir() -> PathBuf {
    fbuild_core::response_file::windows_temp_dir()
}

/// Write flags to a temporary GCC response file (`@file` syntax).
///
/// Delegates to [`fbuild_core::response_file::write_response_file`].
pub fn write_response_file(flags: &[String], temp_dir: &Path, prefix: &str) -> Result<PathBuf> {
    fbuild_core::response_file::write_response_file(flags, temp_dir, prefix)
}

/// Prepare compiler flags for direct execution (no response file).
///
/// Delegates to [`fbuild_core::compiler_flags::prepare_flags_for_exec`].
/// See that module for full documentation.
pub fn prepare_flags_for_exec(flags: Vec<String>) -> Vec<String> {
    fbuild_core::compiler_flags::prepare_flags_for_exec(flags)
}

/// Replace backslashes with forward slashes for GCC response files,
/// but preserve `\"` sequences which are intentional escapes in define values.
///
/// Delegates to [`fbuild_core::response_file::replace_path_backslashes`].
pub fn replace_path_backslashes(s: &str) -> String {
    fbuild_core::response_file::replace_path_backslashes(s)
}

/// Build C flags: common_flags + language-specific C flags from MCU config.
pub fn build_c_flags(common_flags: Vec<String>, config: &dyn McuConfig) -> Vec<String> {
    let mut flags = common_flags;
    flags.extend(config.compiler_flags().c.iter().cloned());
    flags
}

/// Build C++ flags: common_flags + language-specific C++ flags from MCU config.
pub fn build_cpp_flags(common_flags: Vec<String>, config: &dyn McuConfig) -> Vec<String> {
    let mut flags = common_flags;
    flags.extend(config.compiler_flags().cxx.iter().cloned());
    flags
}

/// Compile a single source file: assemble flags, handle response files, execute.
///
/// This is the shared core of all platform compilers. Platform-specific
/// differences are expressed through parameters:
/// - `response_file_prefix`: "avr", "teensy", "esp32"
/// - `extra_pre_flags`: additional flags inserted between base flags and extra_flags
///   (ESP32 uses this for include flags deferred from common_flags)
/// - `compiler_cache`: optional zccache path (ESP32 only, None for others)
#[allow(clippy::too_many_arguments)]
pub fn compile_source(
    compiler: &Path,
    source: &Path,
    output: &Path,
    flags: &[String],
    extra_flags: &[String],
    temp_dir: &Path,
    response_file_prefix: &str,
    verbose: bool,
    compiler_cache: Option<&Path>,
    extra_pre_flags: &[String],
) -> Result<CompileResult> {
    use fbuild_core::subprocess::run_command;

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut all_flags: Vec<String> = Vec::new();
    all_flags.extend(flags.iter().cloned());
    all_flags.extend(extra_pre_flags.iter().cloned());
    all_flags.extend(extra_flags.iter().cloned());
    all_flags.extend([
        "-c".to_string(),
        source.to_string_lossy().to_string(),
        "-o".to_string(),
        output.to_string_lossy().to_string(),
    ]);

    // On Windows, write all flags to a response file to avoid command-line
    // length limits and backslash-quote escaping issues with CreateProcessW.
    let args = if cfg!(windows) {
        let response_file = write_response_file(&all_flags, temp_dir, response_file_prefix)?;
        let mut a = Vec::new();
        if let Some(zcc) = compiler_cache {
            a.push(zcc.to_string_lossy().to_string());
        }
        a.push(compiler.to_string_lossy().to_string());
        a.push(format!("@{}", response_file.display()));
        a
    } else {
        let sanitized = prepare_flags_for_exec(all_flags);
        let mut raw_args: Vec<String> = vec![compiler.to_string_lossy().to_string()];
        raw_args.extend(sanitized);
        let raw_refs: Vec<&str> = raw_args.iter().map(|s| s.as_str()).collect();
        crate::zccache::wrap_args(&raw_refs, compiler_cache)
    };

    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    if verbose {
        tracing::info!("compile: {}", args.join(" "));
    }

    let result = run_command(&args_ref, None, None, None)?;

    Ok(CompileResult {
        success: result.success(),
        object_file: output.to_path_buf(),
        stdout: result.stdout,
        stderr: result.stderr,
        exit_code: result.exit_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_define_flags() {
        let base = CompilerBase {
            mcu: "atmega328p".to_string(),
            f_cpu: "16000000L".to_string(),
            defines: {
                let mut d = HashMap::new();
                d.insert("PLATFORMIO".to_string(), "1".to_string());
                d.insert("F_CPU".to_string(), "16000000L".to_string());
                d
            },
            include_dirs: Vec::new(),
            verbose: false,
        };
        let flags = base.build_define_flags();
        assert!(flags.contains(&"-DPLATFORMIO".to_string()));
        assert!(flags.contains(&"-DF_CPU=16000000L".to_string()));
    }

    #[test]
    fn test_build_include_flags() {
        let base = CompilerBase {
            mcu: String::new(),
            f_cpu: String::new(),
            defines: HashMap::new(),
            include_dirs: vec![
                PathBuf::from("/usr/include"),
                PathBuf::from("/opt/avr/include"),
            ],
            verbose: false,
        };
        let flags = base.build_include_flags();
        assert_eq!(flags.len(), 2);
        assert!(flags[0].starts_with("-I"));
    }

    #[test]
    fn test_needs_rebuild_missing_object() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("test.c");
        std::fs::write(&src, "int main() {}").unwrap();
        let obj = tmp.path().join("test.o");
        assert!(CompilerBase::needs_rebuild(&src, &obj));
    }

    #[test]
    fn test_object_path() {
        let path = CompilerBase::object_path(Path::new("main.cpp"), Path::new("/build"));
        assert_eq!(path, PathBuf::from("/build/main.cpp.o"));
    }

    #[test]
    fn test_prepare_flags_for_exec_strips_escaped_quotes() {
        let flags = vec![
            r#"-DARDUINO_BOARD=\"ESP32_DEV\""#.to_string(),
            r#"-DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\""#.to_string(),
            r#"-DIDF_VER=\"v5.3.2\""#.to_string(),
        ];
        let result = prepare_flags_for_exec(flags);
        assert_eq!(result[0], r#"-DARDUINO_BOARD="ESP32_DEV""#);
        assert_eq!(result[1], r#"-DMBEDTLS_CONFIG_FILE="mbedtls/esp_config.h""#);
        assert_eq!(result[2], r#"-DIDF_VER="v5.3.2""#);
    }

    #[test]
    fn test_prepare_flags_for_exec_preserves_normal_flags() {
        let flags = vec![
            "-DPLATFORMIO".to_string(),
            "-DF_CPU=16000000L".to_string(),
            "-I/usr/include".to_string(),
            "-c".to_string(),
            "-Wall".to_string(),
        ];
        let result = prepare_flags_for_exec(flags.clone());
        assert_eq!(result, flags);
    }

    #[test]
    fn test_prepare_flags_for_exec_empty() {
        let result = prepare_flags_for_exec(Vec::new());
        assert!(result.is_empty());
    }

    #[test]
    fn test_prepare_flags_and_response_file_produce_same_define_value() {
        // Both paths must produce the same define value for GCC.
        // Given input: -DFOO=\"bar\"
        // - prepare_flags_for_exec → -DFOO="bar" (argv: GCC sees FOO = "bar")
        // - write_response_file → '-DFOO="bar"' (response file: GCC sees FOO = "bar")
        let input = r#"-DFOO=\"bar\""#.to_string();

        // Direct exec path
        let exec_result = prepare_flags_for_exec(vec![input.clone()]);
        assert_eq!(exec_result[0], r#"-DFOO="bar""#);

        // Response file path
        let tmp = tempfile::TempDir::new().unwrap();
        let rsp = write_response_file(&[input], tmp.path(), "test").unwrap();
        let content = std::fs::read_to_string(rsp).unwrap();
        // Response file wraps in single quotes with unescaped "
        assert_eq!(content, r#"'-DFOO="bar"'"#);
    }
}
