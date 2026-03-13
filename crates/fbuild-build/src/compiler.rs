//! Compiler traits and base implementation.
//!
//! Defines the `Compiler` trait and `CompilerBase` shared logic for
//! building compiler flags, invoking gcc/g++, and detecting rebuilds.

use fbuild_core::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    pub fn build_define_flags(&self) -> Vec<String> {
        self.defines
            .iter()
            .map(|(k, v)| {
                if v == "1" {
                    format!("-D{}", k)
                } else {
                    format!("-D{}={}", k, v)
                }
            })
            .collect()
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
