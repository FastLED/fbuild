//! ESP8266 compiler implementation — Xtensa LX106 GCC.
//!
//! All flags come from the `Esp8266McuConfig` JSON config.
//! No `-mmcu=` flag needed (ESP8266 has a single MCU variant).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::{BuildProfile, Result};

use super::mcu_config::Esp8266McuConfig;
use crate::compiler::{CompileResult, Compiler, CompilerBase, McuConfig as _};
use crate::eh_frame_policy::EhFramePolicy;

/// ESP8266-specific compiler using Xtensa LX106 GCC.
pub struct Esp8266Compiler {
    pub base: CompilerBase,
    gcc_path: PathBuf,
    gxx_path: PathBuf,
    mcu_config: Esp8266McuConfig,
    profile: BuildProfile,
    temp_dir: PathBuf,
    /// PlatformIO `build_unflags`. See FastLED/fbuild#37.
    build_unflags: Vec<String>,
    /// Whether to strip eh_frame unwinding tables. Default `Preserve` so existing
    /// callers see no behavior change; orchestrators set this via
    /// [`Self::with_eh_frame_policy`]. See FastLED/fbuild#244.
    eh_frame_policy: EhFramePolicy,
}

impl Esp8266Compiler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        gxx_path: PathBuf,
        f_cpu: &str,
        defines: HashMap<String, String>,
        include_dirs: Vec<PathBuf>,
        mcu_config: Esp8266McuConfig,
        profile: BuildProfile,
        verbose: bool,
    ) -> Self {
        Self {
            base: CompilerBase {
                mcu: "esp8266".to_string(),
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
        }
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

    /// Build common compiler flags from the MCU config.
    fn common_flags(&self) -> Vec<String> {
        let mut flags = self.mcu_config.compiler_flags.common.clone();

        // Profile-specific flags
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

    fn asm_flags(&self) -> Vec<String> {
        let mut flags = vec![
            "-g".to_string(),
            "-x".to_string(),
            "assembler-with-cpp".to_string(),
            "-MMD".to_string(),
            "-mlongcalls".to_string(),
        ];
        if let Some(toolchain_root) = self.gcc_path.parent().and_then(|bin| bin.parent()) {
            flags.push(format!("-I{}", toolchain_root.join("include").display()));
        }
        flags.extend(self.base.build_define_flags());
        flags.extend(self.base.build_include_flags());
        flags
    }
}

impl Compiler for Esp8266Compiler {
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
            "esp8266",
            self.base.verbose,
            None,
            &[],
        )
    }

    fn compile(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        match source.extension().and_then(|ext| ext.to_str()) {
            Some("c") => self.compile_c(source, output, extra_flags),
            Some("S") | Some("s") => {
                let flags = self.asm_flags();
                self.compile_one(self.gcc_path(), source, output, &flags, extra_flags)
            }
            _ => self.compile_cpp(source, output, extra_flags),
        }
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
    use crate::esp8266::mcu_config::get_esp8266_config;

    fn test_compiler() -> Esp8266Compiler {
        let mut defines = HashMap::new();
        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        defines.insert("F_CPU".to_string(), "80000000L".to_string());
        defines.insert("ARDUINO".to_string(), "10808".to_string());
        defines.insert("ESP8266".to_string(), "1".to_string());

        Esp8266Compiler::new(
            PathBuf::from("/usr/bin/xtensa-lx106-elf-gcc"),
            PathBuf::from("/usr/bin/xtensa-lx106-elf-g++"),
            "80000000L",
            defines,
            vec![
                PathBuf::from("/cores/esp8266"),
                PathBuf::from("/variants/nodemcu"),
            ],
            get_esp8266_config().unwrap(),
            BuildProfile::Release,
            false,
        )
    }

    #[test]
    fn test_common_flags_contain_architecture() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.contains(&"-mlongcalls".to_string()));
        assert!(flags.contains(&"-mtext-section-literals".to_string()));
    }

    #[test]
    fn test_common_flags_contain_defines() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags.iter().any(|f| f == "-DPLATFORMIO"));
        assert!(flags.iter().any(|f| f == "-DF_CPU=80000000L"));
    }

    #[test]
    fn test_common_flags_contain_includes() {
        let compiler = test_compiler();
        let flags = compiler.common_flags();
        assert!(flags
            .iter()
            .any(|f| f.contains("-I") && f.contains("cores/esp8266")));
        assert!(flags
            .iter()
            .any(|f| f.contains("-I") && f.contains("variants/nodemcu")));
    }

    #[test]
    fn test_c_flags_have_c_standard() {
        let compiler = test_compiler();
        let flags = compiler.c_flags();
        assert!(flags.contains(&"-std=gnu17".to_string()));
    }

    #[test]
    fn test_cpp_flags_have_cpp_standard() {
        let compiler = test_compiler();
        let flags = compiler.cpp_flags();
        assert!(flags.contains(&"-std=gnu++17".to_string()));
        assert!(flags.contains(&"-fno-rtti".to_string()));
        assert!(flags.contains(&"-fno-exceptions".to_string()));
    }

    #[test]
    fn test_profiles_applied() {
        let config = get_esp8266_config().unwrap();
        let release = config.get_profile("release").unwrap();
        assert!(release.compile_flags.contains(&"-Os".to_string()));
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
