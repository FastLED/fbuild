//! Compiler traits and base implementation.
//!
//! Defines the `Compiler` trait and `CompilerBase` shared logic for
//! building compiler flags, invoking gcc/g++, and detecting rebuilds.

use fbuild_core::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

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
    /// Compile a C source file to an object file.
    fn compile_c(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult>;

    /// Compile a C++ source file to an object file.
    fn compile_cpp(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult>;

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
/// On MSYS2/Git Bash, `std::env::temp_dir()` returns `/tmp/` which native
/// Windows GCC treats as `C:\tmp\`. Use `LOCALAPPDATA\Temp` instead.
pub fn windows_temp_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var("LOCALAPPDATA")
            .map(|la| PathBuf::from(la).join("Temp"))
            .unwrap_or_else(|_| std::env::temp_dir())
    } else {
        std::env::temp_dir()
    }
}

/// Write flags to a temporary GCC response file (`@file` syntax).
///
/// Returns the path to the response file. Uses an atomic counter for
/// thread-safe unique filenames during parallel compilation.
///
/// Flags containing `\"` (escaped quotes in define values) are wrapped in
/// single quotes with `\"` converted to plain `"` — GCC's response file
/// parser always preserves literal `"` inside single-quoted arguments.
pub fn write_response_file(flags: &[String], temp_dir: &Path, prefix: &str) -> Result<PathBuf> {
    static RSP_COUNTER: AtomicU64 = AtomicU64::new(0);

    std::fs::create_dir_all(temp_dir).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to create temp dir {}: {}",
            temp_dir.display(),
            e
        ))
    })?;

    let counter = RSP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = temp_dir.join(format!(
        "fbuild_{}_{}_{}.rsp",
        prefix,
        std::process::id(),
        counter
    ));

    // GCC treats backslashes in response files as escape characters (\n = newline,
    // \f = formfeed, etc.). Convert to forward slashes for Windows path compatibility,
    // but preserve \" sequences which are intentional escape sequences (e.g., in
    // -DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\").
    //
    // Flags containing \" (escaped quotes in define values like -DARDUINO_BOARD=\"...\")
    // must be wrapped in single quotes with the \" converted to plain " — GCC's
    // response file parser treats \" inconsistently across platforms, but single-quoted
    // arguments always preserve literal " characters.
    let content = flags
        .iter()
        .map(|f| {
            let fwd = replace_path_backslashes(f);
            if fwd.contains("\\\"") {
                let unescaped = fwd.replace("\\\"", "\"");
                format!("'{}'", unescaped)
            } else if fwd.contains(' ') {
                format!("\"{}\"", fwd)
            } else {
                fwd
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, content).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to write response file {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(path)
}

/// Replace backslashes with forward slashes for GCC response files,
/// but preserve `\"` sequences which are intentional escapes in define values.
pub fn replace_path_backslashes(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut result = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'"' {
            result.push('\\');
            result.push('"');
            i += 2;
        } else if bytes[i] == b'\\' {
            result.push('/');
            i += 1;
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
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
}
