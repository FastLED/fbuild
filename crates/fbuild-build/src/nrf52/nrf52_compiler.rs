//! NRF52 ARM Cortex-M4F compiler implementation.
//!
//! Compiles C/C++ source files using arm-none-eabi-gcc/g++ with appropriate
//! flags for Nordic NRF52 boards (ARM Cortex-M4F, hardware FPU).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::{BuildProfile, Result};

use super::mcu_config::Nrf52McuConfig;
use crate::compiler::{CompileResult, Compiler, CompilerBase};

/// NRF52-specific compiler using arm-none-eabi-gcc and arm-none-eabi-g++.
pub struct Nrf52Compiler {
    pub base: CompilerBase,
    gcc_path: PathBuf,
    gxx_path: PathBuf,
    mcu_config: Nrf52McuConfig,
    profile: BuildProfile,
    temp_dir: PathBuf,
}

impl Nrf52Compiler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        gxx_path: PathBuf,
        mcu: &str,
        f_cpu: &str,
        defines: HashMap<String, String>,
        include_dirs: Vec<PathBuf>,
        mcu_config: Nrf52McuConfig,
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
        }
    }

    /// Build the common ARM Cortex-M4F compiler flags.
    fn common_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();
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

impl Compiler for Nrf52Compiler {
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
            "nrf52",
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nrf52::mcu_config::get_nrf52_config_for_mcu;

    fn test_compiler() -> Nrf52Compiler {
        let mut defines = HashMap::new();
        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        defines.insert("F_CPU".to_string(), "64000000L".to_string());
        defines.insert("ARDUINO".to_string(), "10808".to_string());
        defines.insert("NRF52840_XXAA".to_string(), "1".to_string());

        Nrf52Compiler::new(
            PathBuf::from("/usr/bin/arm-none-eabi-gcc"),
            PathBuf::from("/usr/bin/arm-none-eabi-g++"),
            "nrf52840",
            "64000000L",
            defines,
            vec![PathBuf::from("/nrf52/cores")],
            get_nrf52_config_for_mcu("nrf52840").unwrap(),
            BuildProfile::Release,
            false,
        )
    }

    #[test]
    fn test_common_flags_contain_cortex_m4() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.contains(&"-mcpu=cortex-m4".to_string()));
        assert!(flags.contains(&"-mthumb".to_string()));
        assert!(flags.contains(&"-mfloat-abi=hard".to_string()));
        assert!(flags.contains(&"-mfpu=fpv4-sp-d16".to_string()));
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
        assert!(flags.iter().any(|f| f == "-DF_CPU=64000000L"));
        assert!(flags.iter().any(|f| f == "-DNRF52840_XXAA"));
    }

    #[test]
    fn test_common_flags_contain_includes() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags
            .iter()
            .any(|f| f.contains("-I") && f.contains("nrf52")));
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
        assert!(flags.contains(&"-fno-threadsafe-statics".to_string()));
    }
}
