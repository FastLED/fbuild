//! Teensy ARM Cortex-M7 compiler implementation.
//!
//! Compiles C/C++ source files using arm-none-eabi-gcc/g++ with appropriate
//! flags for Teensy 4.x boards (ARM Cortex-M7, hardware FPU).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::Result;

use crate::compiler::{CompileResult, Compiler, CompilerBase};

/// Teensy-specific compiler using arm-none-eabi-gcc and arm-none-eabi-g++.
pub struct TeensyCompiler {
    pub base: CompilerBase,
    gcc_path: PathBuf,
    gxx_path: PathBuf,
}

impl TeensyCompiler {
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

    /// Build the common ARM Cortex-M7 compiler flags.
    fn common_flags(&self) -> Vec<String> {
        let mut flags = vec![
            "-mcpu=cortex-m7".to_string(),
            "-mthumb".to_string(),
            "-mfloat-abi=hard".to_string(),
            "-mfpu=fpv5-d16".to_string(),
            "-Wall".to_string(),
            "-Wextra".to_string(),
            "-Wno-unused-parameter".to_string(),
            "-ffunction-sections".to_string(),
            "-fdata-sections".to_string(),
            "-MMD".to_string(),
        ];

        // Release flags (always optimize for size on embedded)
        flags.extend([
            "-Os".to_string(),
            "-flto=auto".to_string(),
            "-fno-fat-lto-objects".to_string(),
        ]);

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
            "-std=gnu++17".to_string(),
            "-fno-exceptions".to_string(),
            "-fno-rtti".to_string(),
            "-felide-constructors".to_string(),
            "-fno-threadsafe-statics".to_string(),
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

impl Compiler for TeensyCompiler {
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

    fn test_compiler() -> TeensyCompiler {
        let mut defines = HashMap::new();
        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        defines.insert("F_CPU".to_string(), "600000000L".to_string());
        defines.insert("ARDUINO".to_string(), "10819".to_string());
        defines.insert("ARDUINO_TEENSY41".to_string(), "1".to_string());
        defines.insert("__IMXRT1062__".to_string(), "1".to_string());
        defines.insert("TEENSYDUINO".to_string(), "159".to_string());

        TeensyCompiler::new(
            PathBuf::from("/usr/bin/arm-none-eabi-gcc"),
            PathBuf::from("/usr/bin/arm-none-eabi-g++"),
            "imxrt1062",
            "600000000L",
            defines,
            vec![PathBuf::from("/teensy4")],
            false,
        )
    }

    #[test]
    fn test_common_flags_contain_cortex_m7() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.contains(&"-mcpu=cortex-m7".to_string()));
        assert!(flags.contains(&"-mthumb".to_string()));
        assert!(flags.contains(&"-mfloat-abi=hard".to_string()));
        assert!(flags.contains(&"-mfpu=fpv5-d16".to_string()));
    }

    #[test]
    fn test_common_flags_contain_optimization() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.contains(&"-Os".to_string()));
        assert!(flags.contains(&"-flto=auto".to_string()));
    }

    #[test]
    fn test_common_flags_contain_defines() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.iter().any(|f| f == "-DPLATFORMIO"));
        assert!(flags.iter().any(|f| f == "-DF_CPU=600000000L"));
        assert!(flags.iter().any(|f| f == "-D__IMXRT1062__"));
        assert!(flags.iter().any(|f| f == "-DTEENSYDUINO=159"));
    }

    #[test]
    fn test_common_flags_contain_includes() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags
            .iter()
            .any(|f| f.contains("-I") && f.contains("teensy4")));
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
        assert!(flags.contains(&"-std=gnu++17".to_string()));
        assert!(flags.contains(&"-fno-exceptions".to_string()));
        assert!(flags.contains(&"-fno-rtti".to_string()));
        assert!(flags.contains(&"-felide-constructors".to_string()));
        assert!(flags.contains(&"-fno-threadsafe-statics".to_string()));
    }
}
