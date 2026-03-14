//! ESP32 compiler implementation — data-driven from MCU JSON configs.
//!
//! Uses RISC-V or Xtensa GCC depending on the MCU architecture.
//! All flags come from the Esp32McuConfig, not from hardcoded values.
//! On Windows, uses GCC response files (`@file`) for 305+ include paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{BuildProfile, Result};

use crate::compiler::{CompileResult, Compiler, CompilerBase};

use super::mcu_config::Esp32McuConfig;

/// ESP32-specific compiler using RISC-V or Xtensa GCC.
pub struct Esp32Compiler {
    pub base: CompilerBase,
    gcc_path: PathBuf,
    gxx_path: PathBuf,
    /// MCU config drives all flags.
    mcu_config: Esp32McuConfig,
    /// Build profile (release, quick).
    profile: BuildProfile,
}

impl Esp32Compiler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        gxx_path: PathBuf,
        mcu_config: Esp32McuConfig,
        f_cpu: &str,
        defines: HashMap<String, String>,
        include_dirs: Vec<PathBuf>,
        profile: BuildProfile,
        verbose: bool,
    ) -> Self {
        Self {
            base: CompilerBase {
                mcu: mcu_config.mcu.clone(),
                f_cpu: f_cpu.to_string(),
                defines,
                include_dirs,
                verbose,
            },
            gcc_path,
            gxx_path,
            mcu_config,
            profile,
        }
    }

    /// Build common compiler flags from the MCU config.
    fn common_flags(&self) -> Vec<String> {
        let mut flags = self.mcu_config.compiler_flags.common.clone();

        // Add profile-specific compile flags
        let profile_name = match self.profile {
            BuildProfile::Release => "release",
            BuildProfile::Quick => "quick",
        };
        if let Some(profile) = self.mcu_config.get_profile(profile_name) {
            flags.extend(profile.compile_flags.clone());
        }

        flags.extend(self.base.build_define_flags());
        flags
    }

    /// C-specific flags: common + MCU config C flags.
    pub fn c_flags(&self) -> Vec<String> {
        let mut flags = self.common_flags();
        flags.extend(self.mcu_config.compiler_flags.c.clone());
        flags
    }

    /// C++-specific flags: common + MCU config C++ flags.
    pub fn cpp_flags(&self) -> Vec<String> {
        let mut flags = self.common_flags();
        flags.extend(self.mcu_config.compiler_flags.cxx.clone());
        flags
    }

    /// Build include flags, using a response file on Windows if needed.
    fn include_args(&self) -> Result<Vec<String>> {
        let include_flags = self.base.build_include_flags();

        // On Windows, if we have many include paths, use a response file
        // to avoid exceeding the 32KB command line limit.
        if cfg!(windows) && include_flags.len() > 100 {
            let response_file = write_response_file(&include_flags)?;
            Ok(vec![format!("@{}", response_file.display())])
        } else {
            Ok(include_flags)
        }
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

        let include_args = self.include_args()?;

        let mut args: Vec<String> = vec![compiler.to_string_lossy().to_string()];
        args.extend(flags.iter().cloned());
        args.extend(include_args);
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

impl Compiler for Esp32Compiler {
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

/// Write flags to a temporary response file for GCC `@file` syntax.
///
/// Returns the path to the response file. The file is written to the system
/// temp directory and will persist for the duration of the build.
fn write_response_file(flags: &[String]) -> Result<PathBuf> {
    // On MSYS, std::env::temp_dir() returns "/tmp/" which native Windows
    // binaries (like the ESP32 cross-compiler) treat as "C:\tmp\", not the
    // real temp dir. Use LOCALAPPDATA\Temp for a Windows-native path.
    let temp_dir = if cfg!(windows) {
        std::env::var("LOCALAPPDATA")
            .map(|la| std::path::PathBuf::from(la).join("Temp"))
            .unwrap_or_else(|_| std::env::temp_dir())
    } else {
        std::env::temp_dir()
    };
    let path = temp_dir.join(format!("fbuild_esp32_{}.rsp", std::process::id()));
    // GCC treats backslashes in response files as escape characters (\n = newline,
    // \f = formfeed, etc.). Convert to forward slashes for Windows compatibility.
    let content = flags
        .iter()
        .map(|f| f.replace('\\', "/"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esp32::mcu_config::get_mcu_config;

    fn test_compiler(mcu: &str) -> Esp32Compiler {
        let config = get_mcu_config(mcu).unwrap();
        let mut defines = config.defines_map();
        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        defines.insert("F_CPU".to_string(), "160000000L".to_string());

        let prefix = config.toolchain_prefix();
        Esp32Compiler::new(
            PathBuf::from(format!("/usr/bin/{}gcc", prefix)),
            PathBuf::from(format!("/usr/bin/{}g++", prefix)),
            config,
            "160000000L",
            defines,
            vec![PathBuf::from("/framework/cores/esp32")],
            BuildProfile::Release,
            false,
        )
    }

    #[test]
    fn test_c_flags_esp32c6() {
        let compiler = test_compiler("esp32c6");
        let flags = compiler.c_flags();
        // Common flags from config
        assert!(flags.contains(&"-ffunction-sections".to_string()));
        assert!(flags.contains(&"-fdata-sections".to_string()));
        assert!(flags.contains(&"-MMD".to_string()));
        // C-specific flags
        assert!(flags.contains(&"-std=gnu17".to_string()));
        // RISC-V march
        assert!(flags.iter().any(|f| f.starts_with("-march=rv32imac")));
        // Release profile
        assert!(flags.contains(&"-Os".to_string()));
        assert!(flags.contains(&"-flto=auto".to_string()));
    }

    #[test]
    fn test_cpp_flags_esp32c6() {
        let compiler = test_compiler("esp32c6");
        let flags = compiler.cpp_flags();
        assert!(flags.contains(&"-std=gnu++2b".to_string()));
        assert!(flags.contains(&"-fexceptions".to_string()));
        assert!(flags.contains(&"-fno-rtti".to_string()));
        assert!(flags.contains(&"-fuse-cxa-atexit".to_string()));
    }

    #[test]
    fn test_xtensa_flags_esp32() {
        let compiler = test_compiler("esp32");
        let flags = compiler.c_flags();
        assert!(flags.contains(&"-mlongcalls".to_string()));
        // Xtensa ESP32 has no -march
        assert!(!flags.iter().any(|f| f.starts_with("-march=")));
    }

    #[test]
    fn test_defines_in_flags() {
        let compiler = test_compiler("esp32c6");
        let flags = compiler.common_flags();
        assert!(flags.iter().any(|f| f == "-DPLATFORMIO"));
        assert!(flags.iter().any(|f| f == "-DF_CPU=160000000L"));
        assert!(flags.iter().any(|f| f == "-DESP_PLATFORM"));
        assert!(flags.iter().any(|f| f == "-DARDUINO_ARCH_ESP32"));
    }

    #[test]
    fn test_esp32p4_fpu_flags() {
        let compiler = test_compiler("esp32p4");
        let flags = compiler.c_flags();
        assert!(flags.iter().any(|f| f.contains("rv32imafc")));
        assert!(flags.iter().any(|f| f.contains("ilp32f")));
    }

    #[test]
    fn test_include_flags() {
        let compiler = test_compiler("esp32c6");
        let include_args = compiler.include_args().unwrap();
        // With only 1 include dir, should NOT use response file
        assert!(include_args.iter().any(|f| f.contains("-I")));
    }

    #[test]
    fn test_response_file_generation() {
        let flags: Vec<String> = (0..200)
            .map(|i| format!("-I/path/to/include/{}", i))
            .collect();
        let path = write_response_file(&flags).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("-I/path/to/include/0"));
        assert!(content.contains("-I/path/to/include/199"));
        // Cleanup
        let _ = std::fs::remove_file(&path);
    }
}
