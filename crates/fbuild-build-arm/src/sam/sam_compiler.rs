//! SAM ARM Cortex-M3 compiler implementation.
//!
//! Compiles C/C++ source files using arm-none-eabi-gcc/g++ with appropriate
//! flags for Atmel SAM boards (ARM Cortex-M3).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::path::NormalizedPath;
use fbuild_core::{BuildProfile, Result};

use super::mcu_config::SamMcuConfig;
use crate::compiler::{CompileResult, Compiler, CompilerBase};

/// SAM-specific compiler using arm-none-eabi-gcc and arm-none-eabi-g++.
pub struct SamCompiler {
    pub base: CompilerBase,
    gcc_path: PathBuf,
    gxx_path: PathBuf,
    mcu_config: SamMcuConfig,
    profile: BuildProfile,
    temp_dir: PathBuf,
    /// PlatformIO `build_unflags`. See FastLED/fbuild#37.
    build_unflags: Vec<String>,
    cxx_system_includes: Vec<NormalizedPath>,
}

impl SamCompiler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        gxx_path: PathBuf,
        mcu: &str,
        f_cpu: &str,
        defines: HashMap<String, String>,
        include_dirs: Vec<PathBuf>,
        mcu_config: SamMcuConfig,
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
            cxx_system_includes: Vec::new(),
        }
    }

    /// Attach PlatformIO `build_unflags`. See FastLED/fbuild#37.
    pub fn with_build_unflags(mut self, build_unflags: Vec<String>) -> Self {
        self.build_unflags = build_unflags;
        self
    }

    /// Resolve the actual target's standard headers once, for core and libraries.
    pub async fn resolve_cxx_system_includes(
        &mut self,
        extra: &[String],
        cwd: &Path,
    ) -> Result<()> {
        let base_flags = crate::compiler::build_cpp_flags(self.common_flags(), &self.mcu_config);
        let (mut flags, extra) =
            crate::compiler::apply_compile_unflags(base_flags, extra, &self.build_unflags);
        flags.extend(extra);
        self.cxx_system_includes =
            fbuild_packages::toolchain::gcc::cxx_system_includes(&self.gxx_path, &flags, cwd)
                .await?;
        Ok(())
    }

    /// Build the common ARM Cortex-M3 compiler flags.
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

#[async_trait::async_trait]
impl Compiler for SamCompiler {
    async fn compile_one(
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
            "sam",
            self.base.verbose,
            None,
            &[],
        )
        .await
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
        let mut flags = crate::compiler::build_cpp_flags(self.common_flags(), &self.mcu_config);
        if !self.cxx_system_includes.is_empty() {
            // Replace the complete builtin list: GCC must not retain a long
            // duplicate spelling in preference to the canonical -isystem path.
            flags.push("-nostdinc".into());
        }
        for directory in &self.cxx_system_includes {
            flags.extend(["-isystem".into(), directory.display_slash()]);
        }
        flags
    }

    fn build_unflags(&self) -> &[String] {
        &self.build_unflags
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sam::mcu_config::get_sam_config_for_mcu;

    fn test_compiler() -> SamCompiler {
        let mut defines = HashMap::new();
        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        defines.insert("F_CPU".to_string(), "84000000L".to_string());
        defines.insert("ARDUINO".to_string(), "10808".to_string());

        SamCompiler::new(
            PathBuf::from("/usr/bin/arm-none-eabi-gcc"),
            PathBuf::from("/usr/bin/arm-none-eabi-g++"),
            "at91sam3x8e",
            "84000000L",
            defines,
            vec![PathBuf::from("/sam/cores")],
            get_sam_config_for_mcu("at91sam3x8e").unwrap(),
            BuildProfile::Release,
            false,
        )
    }

    #[test]
    fn canonical_cxx_headers_do_not_enter_c_compilation() {
        let mut compiler = test_compiler();
        let directory = NormalizedPath::from("/canonical sdk/headers");
        compiler.cxx_system_includes = vec![directory.clone()];
        let flags = compiler.cpp_flags();
        let index = flags.iter().position(|flag| flag == "-isystem").unwrap();
        assert_eq!(flags[index + 1], directory.display_slash());
        assert!(flags.iter().any(|flag| flag == "-nostdinc"));
        assert!(
            !compiler
                .c_flags()
                .iter()
                .any(|flag| flag == "-isystem" || flag == "-nostdinc")
        );
    }

    #[test]
    fn test_common_flags_contain_cortex_m3() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.contains(&"-mcpu=cortex-m3".to_string()));
        assert!(flags.contains(&"-mthumb".to_string()));
    }

    #[test]
    fn test_common_flags_contain_optimization() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.contains(&"-Os".to_string()));
    }

    #[test]
    fn test_common_flags_contain_defines() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.iter().any(|f| f == "-DPLATFORMIO"));
        assert!(flags.iter().any(|f| f == "-DF_CPU=84000000L"));
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
