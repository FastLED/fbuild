//! ESP32 compiler implementation — data-driven from MCU JSON configs.
//!
//! Uses RISC-V or Xtensa GCC depending on the MCU architecture.
//! All flags come from the Esp32McuConfig, not from hardcoded values.
//! On Windows, uses GCC response files (`@file`) for 305+ include paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::{BuildProfile, Result};

use crate::compiler::{CompileResult, Compiler, CompilerBase};
use crate::eh_frame_policy::EhFramePolicy;

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
    /// Directory for temporary files (response files, etc.).
    temp_dir: PathBuf,
    /// Optional zccache path for compiler caching.
    compiler_cache: Option<PathBuf>,
    /// PlatformIO `build_unflags` tokens to strip from the effective
    /// compile line. Populated post-construction by the orchestrator
    /// from `BuildContext::build_unflags`. Empty by default so existing
    /// callers don't need to change. See FastLED/fbuild#37.
    build_unflags: Vec<String>,
    /// Whether to strip eh_frame unwinding tables. Default `Preserve` so existing
    /// callers see no behavior change; orchestrators set this via
    /// [`Self::with_eh_frame_policy`]. See FastLED/fbuild#243.
    eh_frame_policy: EhFramePolicy,
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
        Self::with_temp_dir(
            gcc_path,
            gxx_path,
            mcu_config,
            f_cpu,
            defines,
            include_dirs,
            profile,
            verbose,
            // On MSYS2/Git Bash, std::env::temp_dir() returns "/tmp/" which
            // native Windows GCC treats as "C:\tmp\". Use LOCALAPPDATA\Temp.
            if cfg!(windows) {
                std::env::var("LOCALAPPDATA")
                    .map(|la| PathBuf::from(la).join("Temp"))
                    .unwrap_or_else(|_| std::env::temp_dir())
            } else {
                std::env::temp_dir()
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_temp_dir(
        gcc_path: PathBuf,
        gxx_path: PathBuf,
        mcu_config: Esp32McuConfig,
        f_cpu: &str,
        defines: HashMap<String, String>,
        include_dirs: Vec<PathBuf>,
        profile: BuildProfile,
        verbose: bool,
        temp_dir: PathBuf,
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
            temp_dir,
            compiler_cache: None,
            build_unflags: Vec::new(),
            eh_frame_policy: EhFramePolicy::default(),
        }
    }

    /// Attach PlatformIO `build_unflags` to be stripped from every compile
    /// command issued by this compiler. Consumed by the default `compile_c` /
    /// `compile_cpp` impls via `Compiler::build_unflags`, so the
    /// framework-level flags (not just user flags) are filtered too.
    /// See FastLED/fbuild#37.
    pub fn with_build_unflags(mut self, build_unflags: Vec<String>) -> Self {
        self.build_unflags = build_unflags;
        self
    }

    /// Attach the eh_frame strip/preserve policy decided by the orchestrator.
    /// Default `Preserve` keeps existing behavior; set `Strip` to drop the
    /// unwind tables via `-fno-asynchronous-unwind-tables -fno-unwind-tables`.
    /// See FastLED/fbuild#243.
    pub fn with_eh_frame_policy(mut self, policy: EhFramePolicy) -> Self {
        self.eh_frame_policy = policy;
        self
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

        // mbedtls and other compat defines from the data-driven JSON config
        flags.extend(self.mcu_config.compat_define_flags());

        flags.extend(self.base.build_define_flags());

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
impl Compiler for Esp32Compiler {
    async fn compile_one(
        &self,
        compiler_path: &Path,
        source: &Path,
        output: &Path,
        flags: &[String],
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        let include_flags = self.base.build_include_flags();
        crate::compiler::compile_source(
            compiler_path,
            source,
            output,
            flags,
            extra_flags,
            &self.temp_dir,
            "esp32",
            self.base.verbose,
            self.compiler_cache.as_deref(),
            &include_flags,
        )
        .await
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

    fn rebuild_signature(&self, source: &Path, extra_flags: &[String]) -> String {
        let ext = source
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let base_flags = match ext.as_str() {
            "c" | "s" => self.c_flags(),
            _ => self.cpp_flags(),
        };
        let include_flags = self.base.build_include_flags();
        let compiler_path = match ext.as_str() {
            "c" | "s" => self.gcc_path(),
            _ => self.gxx_path(),
        };
        crate::compiler::build_rebuild_signature(
            compiler_path,
            &base_flags,
            &include_flags,
            extra_flags,
        )
    }
}

// Response file utilities (write_response_file, replace_path_backslashes)
// are in crate::compiler for shared use across all platform compilers.

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
        assert!(flags.iter().any(|f| f == "-DARDUINO_ARCH_ESP32"));
        // SDK-owned macros like ESP_PLATFORM are supplied later by the framework.
        assert!(!flags.iter().any(|f| f == "-DESP_PLATFORM"));
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
        let include_flags = compiler.base.build_include_flags();
        // With only 1 include dir, should have -I flags
        assert!(include_flags.iter().any(|f: &String| f.contains("-I")));
    }

    #[tokio::test]
    async fn test_response_file_generation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let flags: Vec<String> = (0..200)
            .map(|i| format!("-I/path/to/include/{}", i))
            .collect();
        let path = crate::compiler::write_response_file(&flags, tmp.path(), "esp32")
            .await
            .unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("-I/path/to/include/0"));
        assert!(content.contains("-I/path/to/include/199"));
    }

    /// Regression guard for FastLED/fbuild#37: with `build_unflags`
    /// populated, a framework-contributed flag (e.g. `-std=gnu++2b`)
    /// must be removed from the effective compile line. The trait's
    /// default `compile_c`/`compile_cpp` route through
    /// `Compiler::build_unflags` → `apply_compile_unflags`, so we can
    /// verify by checking that the ESP32 compiler reports the
    /// configured unflags back and that removing the framework default
    /// via `remove_unflagged_tokens` leaves the right residue.
    #[test]
    fn with_build_unflags_exposes_them_via_trait_method() {
        let compiler = test_compiler("esp32c6")
            .with_build_unflags(vec!["-std=gnu++2b".to_string(), "-Os".to_string()]);
        assert_eq!(
            Compiler::build_unflags(&compiler),
            &["-std=gnu++2b".to_string(), "-Os".to_string()]
        );
    }

    /// Default trait impl returns an empty slice when `with_build_unflags`
    /// was never called — guarantees zero behavior change for callers
    /// that haven't opted in.
    #[test]
    fn default_build_unflags_is_empty() {
        let compiler = test_compiler("esp32c6");
        assert!(Compiler::build_unflags(&compiler).is_empty());
    }

    /// End-to-end check that the unflags set is actually applied to the
    /// platform-level `cpp_flags()` when routed through the trait's
    /// default compile path. We can't invoke the real compile (no
    /// toolchain in tests) but we can mirror the filter the default
    /// impl uses and confirm it drops the flag.
    #[test]
    fn configured_unflags_strip_framework_cpp_flag() {
        let compiler =
            test_compiler("esp32c6").with_build_unflags(vec!["-std=gnu++2b".to_string()]);
        let mut flags = compiler.cpp_flags();
        assert!(
            flags.contains(&"-std=gnu++2b".to_string()),
            "precondition: framework provides -std=gnu++2b"
        );
        crate::pipeline::remove_unflagged_tokens(&mut flags, Compiler::build_unflags(&compiler));
        assert!(
            !flags.contains(&"-std=gnu++2b".to_string()),
            "unflags must strip framework-contributed -std=gnu++2b"
        );
    }

    /// FastLED/fbuild#243: by default the compiler preserves eh_frame; the
    /// STRIP_FLAGS must not leak into the effective compile line.
    #[test]
    fn cpp_flags_preserve_eh_frame_by_default() {
        let compiler = test_compiler("esp32c6");
        let flags = compiler.cpp_flags();
        assert!(!flags.iter().any(|f| f == "-fno-asynchronous-unwind-tables"));
        assert!(!flags.iter().any(|f| f == "-fno-unwind-tables"));
    }

    /// FastLED/fbuild#243: when policy is Strip, both unwind-tables flags
    /// must appear in the effective C++ flag list.
    #[test]
    fn cpp_flags_strip_eh_frame_when_policy_set() {
        let compiler = test_compiler("esp32c6").with_eh_frame_policy(EhFramePolicy::Strip);
        let flags = compiler.cpp_flags();
        assert!(flags.iter().any(|f| f == "-fno-asynchronous-unwind-tables"));
        assert!(flags.iter().any(|f| f == "-fno-unwind-tables"));
    }

    #[test]
    fn test_mbedtls_compat_defines_in_flags() {
        let compiler = test_compiler("esp32c6");
        let flags = compiler.common_flags();
        assert!(flags
            .iter()
            .any(|f| f == "-Dmbedtls_md5_starts_ret=mbedtls_md5_starts"));
        assert!(flags
            .iter()
            .any(|f| f == "-Dmbedtls_sha1_finish_ret=mbedtls_sha1_finish"));
    }
}
