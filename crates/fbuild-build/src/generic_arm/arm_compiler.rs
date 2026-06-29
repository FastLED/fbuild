//! Generic ARM Cortex-M compiler implementation.
//!
//! Compiles C/C++ source files using arm-none-eabi-gcc/g++ with appropriate
//! flags for any ARM Cortex-M MCU (STM32, RP2040, NRF52, SAM, etc.).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::{BuildProfile, Result};

use super::mcu_config::ArmMcuConfig;
use crate::compiler::{CompileResult, Compiler, CompilerBase};
use crate::eh_frame_policy::EhFramePolicy;

/// Generic ARM compiler using arm-none-eabi-gcc and arm-none-eabi-g++.
pub struct ArmCompiler {
    pub base: CompilerBase,
    gcc_path: PathBuf,
    gxx_path: PathBuf,
    mcu_config: ArmMcuConfig,
    profile: BuildProfile,
    temp_dir: PathBuf,
    /// PlatformIO `build_unflags` to strip from the effective compile
    /// line. See FastLED/fbuild#37.
    build_unflags: Vec<String>,
    /// Whether to strip eh_frame unwinding tables. Default `Preserve` so existing
    /// callers see no behavior change; orchestrators set this via
    /// [`Self::with_eh_frame_policy`]. See FastLED/fbuild#244.
    eh_frame_policy: EhFramePolicy,
    /// Dead pass-through (FastLED/fbuild#800). The wrapper-binary
    /// `zccache wrap <gcc>` path was deleted; every compile dispatches
    /// through the embedded `ZccacheService::compile` instead. Field
    /// kept as `None` to preserve the per-platform compiler API
    /// surface until a future PR rewrites those signatures.
    compiler_cache: Option<PathBuf>,
}

impl ArmCompiler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        gxx_path: PathBuf,
        mcu: &str,
        f_cpu: &str,
        defines: HashMap<String, String>,
        include_dirs: Vec<PathBuf>,
        mcu_config: ArmMcuConfig,
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
            eh_frame_policy: EhFramePolicy::default(),
            compiler_cache: None,
        }
    }

    /// Override the auto-discovered compiler cache. Pass `None` to
    /// explicitly opt out of zccache wrapping even when zccache is on
    /// PATH. Primarily for tests / benchmarks; production callers should
    /// rely on the auto-discovery in [`Self::new`].
    pub fn with_compiler_cache(mut self, compiler_cache: Option<PathBuf>) -> Self {
        self.compiler_cache = compiler_cache;
        self
    }

    /// Attach PlatformIO `build_unflags`. See FastLED/fbuild#37.
    pub fn with_build_unflags(mut self, build_unflags: Vec<String>) -> Self {
        self.build_unflags = build_unflags;
        self
    }

    /// Attach the eh_frame strip/preserve policy decided by the orchestrator.
    /// Default `Preserve` keeps existing behavior. See FastLED/fbuild#244.
    pub fn with_eh_frame_policy(mut self, policy: EhFramePolicy) -> Self {
        self.eh_frame_policy = policy;
        self
    }

    /// Build the common ARM Cortex-M compiler flags.
    fn common_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();
        flags.extend(self.mcu_config.compiler_flags.common.iter().cloned());

        // Profile-specific flags (optimization, LTO, etc.)
        if let Some(profile) = self.mcu_config.get_profile(self.profile.as_dir_name()) {
            flags.extend(profile.compile_flags.iter().cloned());
        }

        flags.extend(self.base.build_define_flags());
        flags.extend(self.base.build_include_flags());

        if matches!(self.eh_frame_policy, EhFramePolicy::Strip) {
            flags.extend(
                crate::eh_frame_policy::STRIP_FLAGS
                    .iter()
                    .map(|s| s.to_string()),
            );
        }
        flags
    }
}

#[async_trait::async_trait]
impl Compiler for ArmCompiler {
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
            "arm",
            self.base.verbose,
            self.compiler_cache.as_deref(),
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
        crate::compiler::build_cpp_flags(self.common_flags(), &self.mcu_config)
    }

    fn build_unflags(&self) -> &[String] {
        &self.build_unflags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ArmMcuConfig {
        serde_json::from_str(
            r#"{
                "name": "TestARM",
                "architecture": "arm-cortex-m3",
                "compiler_flags": {
                    "common": ["-mcpu=cortex-m3", "-mthumb", "-Wall"],
                    "c": ["-std=gnu11"],
                    "cxx": ["-std=gnu++17", "-fno-exceptions", "-fno-rtti", "-fno-threadsafe-statics"]
                },
                "linker_flags": ["-mcpu=cortex-m3", "-mthumb"],
                "linker_libs": ["-lgcc", "-lm"],
                "objcopy": {"output_format": "ihex", "remove_sections": [".eeprom"]},
                "profiles": {
                    "release": {"compile_flags": ["-Os", "-flto"], "link_flags": ["-flto"]},
                    "quick": {"compile_flags": ["-Os"], "link_flags": []}
                },
                "defines": [["ARDUINO", "10808"]]
            }"#,
        )
        .unwrap()
    }

    fn test_compiler() -> ArmCompiler {
        let mut defines = HashMap::new();
        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        defines.insert("F_CPU".to_string(), "72000000L".to_string());

        ArmCompiler::new(
            PathBuf::from("/usr/bin/arm-none-eabi-gcc"),
            PathBuf::from("/usr/bin/arm-none-eabi-g++"),
            "stm32f103",
            "72000000L",
            defines,
            vec![PathBuf::from("/cores/arduino")],
            test_config(),
            BuildProfile::Release,
            false,
        )
    }

    #[test]
    fn test_common_flags_contain_cpu() {
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
        assert!(flags.contains(&"-flto".to_string()));
    }

    #[test]
    fn test_common_flags_contain_defines() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.iter().any(|f| f == "-DPLATFORMIO"));
        assert!(flags.iter().any(|f| f == "-DF_CPU=72000000L"));
    }

    #[test]
    fn test_common_flags_contain_includes() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags
            .iter()
            .any(|f| f.contains("-I") && f.contains("cores")));
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

    /// FastLED/fbuild#244: default policy must not leak STRIP_FLAGS.
    #[test]
    fn cpp_flags_preserve_eh_frame_by_default() {
        let compiler = test_compiler();
        let flags = compiler.cpp_flags();
        assert!(!flags.iter().any(|f| f == "-fno-asynchronous-unwind-tables"));
        assert!(!flags.iter().any(|f| f == "-fno-unwind-tables"));
    }

    /// FastLED/fbuild#244: Strip policy must inject both unwind-table flags.
    #[test]
    fn cpp_flags_strip_eh_frame_when_policy_set() {
        let compiler = test_compiler().with_eh_frame_policy(EhFramePolicy::Strip);
        let flags = compiler.cpp_flags();
        assert!(flags.iter().any(|f| f == "-fno-asynchronous-unwind-tables"));
        assert!(flags.iter().any(|f| f == "-fno-unwind-tables"));
    }
}
