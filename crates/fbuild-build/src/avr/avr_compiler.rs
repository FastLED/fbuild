//! AVR-GCC compiler implementation.
//!
//! Compiles C/C++ source files using avr-gcc/avr-g++ with appropriate
//! MCU flags, defines, and include paths for Arduino AVR boards.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::Result;

use crate::compiler::{CompileResult, Compiler, CompilerBase};

/// AVR-specific compiler using avr-gcc and avr-g++.
pub struct AvrCompiler {
    pub base: CompilerBase,
    gcc_path: PathBuf,
    gxx_path: PathBuf,
}

impl AvrCompiler {
    pub fn new(
        gcc_path: PathBuf,
        gxx_path: PathBuf,
        mcu: &str,
        f_cpu: &str,
        defines: HashMap<String, String>,
        include_dirs: Vec<PathBuf>,
        verbose: bool,
    ) -> Self {
        Self {
            base: CompilerBase {
                mcu: mcu.to_string(),
                f_cpu: f_cpu.to_string(),
                defines,
                include_dirs,
                verbose,
            },
            gcc_path,
            gxx_path,
        }
    }

    /// Build the common AVR compiler flags.
    fn common_flags(&self) -> Vec<String> {
        let mut flags = vec![
            format!("-mmcu={}", self.base.mcu),
            "-Os".to_string(),
            "-Wall".to_string(),
            "-ffunction-sections".to_string(),
            "-fdata-sections".to_string(),
            "-flto".to_string(),
            "-fno-fat-lto-objects".to_string(),
        ];

        flags.extend(self.base.build_define_flags());
        flags.extend(self.base.build_include_flags());
        flags
    }

    /// C-specific flags.
    fn c_flags(&self) -> Vec<String> {
        let mut flags = self.common_flags();
        flags.push("-std=gnu11".to_string());
        flags
    }

    /// C++-specific flags.
    fn cpp_flags(&self) -> Vec<String> {
        let mut flags = self.common_flags();
        flags.extend([
            "-std=gnu++11".to_string(),
            "-fno-exceptions".to_string(),
            "-fno-threadsafe-statics".to_string(),
            "-fpermissive".to_string(),
        ]);
        flags
    }

    /// Compile a single source file using the given compiler and flags.
    fn compile_with(
        &self,
        compiler: &Path,
        source: &Path,
        output: &Path,
        flags: &[String],
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        // Ensure output directory exists
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut args: Vec<String> = vec![compiler.to_string_lossy().to_string()];
        args.extend(flags.iter().cloned());
        args.extend(extra_flags.iter().cloned());
        args.extend([
            "-c".to_string(),
            source.to_string_lossy().to_string(),
            "-o".to_string(),
            output.to_string_lossy().to_string(),
        ]);

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        if self.base.verbose {
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
}

impl Compiler for AvrCompiler {
    fn compile_c(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        let flags = self.c_flags();
        self.compile_with(&self.gcc_path, source, output, &flags, extra_flags)
    }

    fn compile_cpp(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        let flags = self.cpp_flags();
        self.compile_with(&self.gxx_path, source, output, &flags, extra_flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_compiler() -> AvrCompiler {
        let mut defines = HashMap::new();
        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        defines.insert("F_CPU".to_string(), "16000000L".to_string());
        defines.insert("ARDUINO".to_string(), "10808".to_string());
        defines.insert("ARDUINO_AVR_UNO".to_string(), "1".to_string());

        AvrCompiler::new(
            PathBuf::from("/usr/bin/avr-gcc"),
            PathBuf::from("/usr/bin/avr-g++"),
            "atmega328p",
            "16000000L",
            defines,
            vec![
                PathBuf::from("/cores/arduino"),
                PathBuf::from("/variants/standard"),
            ],
            false,
        )
    }

    #[test]
    fn test_common_flags_contain_mcu() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.contains(&"-mmcu=atmega328p".to_string()));
    }

    #[test]
    fn test_common_flags_contain_optimization() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.contains(&"-Os".to_string()));
        assert!(flags.contains(&"-flto".to_string()));
    }

    #[test]
    fn test_common_flags_contain_defines() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.iter().any(|f| f == "-DPLATFORMIO"));
        assert!(flags.iter().any(|f| f == "-DF_CPU=16000000L"));
    }

    #[test]
    fn test_common_flags_contain_includes() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags
            .iter()
            .any(|f| f.contains("-I") && f.contains("cores/arduino")));
        assert!(flags
            .iter()
            .any(|f| f.contains("-I") && f.contains("variants/standard")));
    }

    #[test]
    fn test_c_flags_have_c_standard() {
        let compiler = test_compiler();
        let flags = compiler.c_flags();
        assert!(flags.contains(&"-std=gnu11".to_string()));
    }

    #[test]
    fn test_cpp_flags_have_cpp_standard() {
        let compiler = test_compiler();
        let flags = compiler.cpp_flags();
        assert!(flags.contains(&"-std=gnu++11".to_string()));
        assert!(flags.contains(&"-fno-exceptions".to_string()));
    }
}
