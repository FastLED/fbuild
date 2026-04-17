//! CH32V RISC-V compiler implementation.
//!
//! Compiles C/C++ source files using riscv-none-elf-gcc/g++ with appropriate
//! flags for CH32V boards (RISC-V RV32EC/RV32IMAC).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::{BuildProfile, Result};

use super::mcu_config::Ch32vMcuConfig;
use crate::compiler::{CompileResult, Compiler, CompilerBase};

/// CH32V-specific compiler using riscv-none-elf-gcc and riscv-none-elf-g++.
pub struct Ch32vCompiler {
    pub base: CompilerBase,
    gcc_path: PathBuf,
    gxx_path: PathBuf,
    mcu_config: Ch32vMcuConfig,
    profile: BuildProfile,
    temp_dir: PathBuf,
    /// Extra flags prepended to every compile (e.g. `-isystem` for multilib).
    extra_pre_flags: Vec<String>,
    /// PlatformIO `build_unflags`. See FastLED/fbuild#37.
    build_unflags: Vec<String>,
}

impl Ch32vCompiler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        gxx_path: PathBuf,
        mcu: &str,
        f_cpu: &str,
        defines: HashMap<String, String>,
        include_dirs: Vec<PathBuf>,
        mcu_config: Ch32vMcuConfig,
        profile: BuildProfile,
        verbose: bool,
        extra_pre_flags: Vec<String>,
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
            extra_pre_flags,
            build_unflags: Vec::new(),
        }
    }

    /// Attach PlatformIO `build_unflags`. See FastLED/fbuild#37.
    pub fn with_build_unflags(mut self, build_unflags: Vec<String>) -> Self {
        self.build_unflags = build_unflags;
        self
    }

    /// Build the common RISC-V compiler flags.
    fn common_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();
        flags.extend(self.mcu_config.compiler_flags.common.iter().cloned());

        // Profile-specific flags (optimization, LTO, etc.)
        if let Some(profile) = self.mcu_config.get_profile(self.profile.as_dir_name()) {
            flags.extend(profile.compile_flags.iter().cloned());
        }

        flags.extend(self.extra_pre_flags.iter().cloned());
        flags.extend(self.base.build_define_flags());
        flags.extend(self.base.build_include_flags());
        flags
    }
}

impl Compiler for Ch32vCompiler {
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
            "ch32v",
            self.base.verbose,
            None,
            &[],
        )
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

    fn build_unflags(&self) -> &[String] {
        &self.build_unflags
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ch32v::mcu_config::get_ch32v_config_for_mcu;

    fn test_compiler() -> Ch32vCompiler {
        let mut defines = HashMap::new();
        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        defines.insert("F_CPU".to_string(), "48000000L".to_string());
        defines.insert("ARDUINO".to_string(), "10808".to_string());

        Ch32vCompiler::new(
            PathBuf::from("/usr/bin/riscv-none-elf-gcc"),
            PathBuf::from("/usr/bin/riscv-none-elf-g++"),
            "ch32v003",
            "48000000L",
            defines,
            vec![PathBuf::from("/ch32v/cores")],
            get_ch32v_config_for_mcu("ch32v003").unwrap(),
            BuildProfile::Release,
            false,
            Vec::new(),
        )
    }

    #[test]
    fn test_common_flags_contain_riscv() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.contains(&"-march=rv32ec_zicsr".to_string()));
        assert!(flags.contains(&"-mabi=ilp32e".to_string()));
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
        assert!(flags.iter().any(|f| f == "-DF_CPU=48000000L"));
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
    }
}
