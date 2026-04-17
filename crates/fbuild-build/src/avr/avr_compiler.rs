//! AVR-GCC compiler implementation.
//!
//! Compiles C/C++ source files using avr-gcc/avr-g++ with appropriate
//! MCU flags, defines, and include paths for Arduino AVR boards.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::{BuildProfile, Result};

use super::mcu_config::AvrMcuConfig;
use crate::compiler::{CompileResult, Compiler, CompilerBase};

/// AVR-specific compiler using avr-gcc and avr-g++.
pub struct AvrCompiler {
    pub base: CompilerBase,
    gcc_path: PathBuf,
    gxx_path: PathBuf,
    mcu_config: AvrMcuConfig,
    profile: BuildProfile,
    temp_dir: PathBuf,
    /// PlatformIO `build_unflags` to strip from the effective compile
    /// line. See FastLED/fbuild#37.
    build_unflags: Vec<String>,
}

impl AvrCompiler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        gxx_path: PathBuf,
        mcu: &str,
        f_cpu: &str,
        defines: HashMap<String, String>,
        include_dirs: Vec<PathBuf>,
        mcu_config: AvrMcuConfig,
        profile: BuildProfile,
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
            mcu_config,
            profile,
            temp_dir: fbuild_core::response_file::windows_temp_dir(),
            build_unflags: Vec::new(),
        }
    }

    /// Attach PlatformIO `build_unflags` to strip from every compile
    /// command. See FastLED/fbuild#37.
    pub fn with_build_unflags(mut self, build_unflags: Vec<String>) -> Self {
        self.build_unflags = build_unflags;
        self
    }

    /// Build the common AVR compiler flags.
    fn common_flags(&self) -> Vec<String> {
        let mut flags = vec![format!("-mmcu={}", self.base.mcu)];
        flags.extend(self.mcu_config.compiler_flags.common.iter().cloned());

        // Profile-specific flags (optimization, LTO, etc.)
        if let Some(profile) = self.mcu_config.get_profile(self.profile.as_dir_name()) {
            flags.extend(profile.compile_flags.iter().cloned());
        }

        flags.extend(self.base.build_define_flags());
        flags.extend(self.base.build_include_flags());
        flags
    }
}

impl Compiler for AvrCompiler {
    fn compile_one(
        &self,
        compiler_path: &Path,
        source: &Path,
        output: &Path,
        flags: &[String],
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        crate::compiler::compile_source(
            compiler_path,
            source,
            output,
            flags,
            extra_flags,
            &self.temp_dir,
            "avr",
            self.base.verbose,
            None,
            &[],
        )
    }

    fn build_unflags(&self) -> &[String] {
        &self.build_unflags
    }

    fn gcc_path(&self) -> &Path {
        &self.gcc_path
    }

    fn gxx_path(&self) -> &Path {
        &self.gxx_path
    }

    fn c_flags(&self) -> Vec<String> {
        crate::compiler::build_c_flags(self.common_flags(), &self.mcu_config)
    }

    fn cpp_flags(&self) -> Vec<String> {
        crate::compiler::build_cpp_flags(self.common_flags(), &self.mcu_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avr::mcu_config::get_avr_config;

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
            get_avr_config().unwrap(),
            BuildProfile::Release,
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
