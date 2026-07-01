//! CH32V RISC-V compiler implementation.
//!
//! Compiles C/C++ source files using riscv-none-elf-gcc/g++ with appropriate
//! flags for CH32V boards (RISC-V RV32EC/RV32IMAC).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::path::NormalizedPath;
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
    /// Root of the third-party OpenWCH core install. Sources rooted under this
    /// path get warnings silenced — we don't own that code, and the pinned
    /// `arduino_core_ch32` 1.0.4 release does not build cleanly under our
    /// `-Wall -Wextra` policy. User sketch sources and project libraries are
    /// not affected. See FastLED/fbuild#382.
    framework_root: Option<PathBuf>,
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
            framework_root: None,
        }
    }

    /// Attach PlatformIO `build_unflags`. See FastLED/fbuild#37.
    pub fn with_build_unflags(mut self, build_unflags: Vec<String>) -> Self {
        self.build_unflags = build_unflags;
        self
    }

    /// Scope warning suppressions to sources under `root`.
    ///
    /// Used to silence diagnostics from the pinned OpenWCH `arduino_core_ch32`
    /// release without hiding warnings on user/sketch code. See
    /// FastLED/fbuild#382.
    pub fn with_framework_root(mut self, root: PathBuf) -> Self {
        self.framework_root = Some(root);
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

    /// Return `true` if `source` lives under the configured framework root.
    fn is_framework_source(&self, source: &Path) -> bool {
        let Some(root) = self.framework_root.as_ref() else {
            return false;
        };
        let source = NormalizedPath::new(source).into_path_buf();
        let root = NormalizedPath::new(root).into_path_buf();
        source.starts_with(&root)
    }
}

/// Flags appended when compiling third-party OpenWCH core/variant sources.
///
/// fbuild's CH32V common flags include `-Wall -Wextra`, which the pinned
/// upstream `arduino_core_ch32` 1.0.4 release does not build cleanly under.
/// One of the diagnostics it emits — "excess elements in struct initializer"
/// — has no dedicated `-Wno-*` toggle (it's hard-wired on in GCC, independent
/// of `-Wall`/`-Wextra`), so the only way to make a clean build is the blanket
/// `-w`. Scope: framework sources only. User sketch and project libraries
/// still compile under the full `-Wall -Wextra` policy, so new diagnostics on
/// user code are not hidden. See FastLED/fbuild#382.
fn framework_suppression_flags() -> &'static [&'static str] {
    &["-w"]
}

#[async_trait::async_trait]
impl Compiler for Ch32vCompiler {
    async fn compile_one(
        &self,
        compiler_path: &Path,
        source: &Path,
        output: &Path,
        flags: &[String],
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        // Append framework warning suppressions after user-provided flags so
        // they override any earlier `-W` toggles. Scoped strictly to sources
        // rooted in the OpenWCH core/variant install; sketch sources and
        // project libraries continue to see the full `-Wall -Wextra` set.
        let suppressed_extra: Vec<String>;
        let effective_extra: &[String] = if self.is_framework_source(source) {
            suppressed_extra = extra_flags
                .iter()
                .cloned()
                .chain(
                    framework_suppression_flags()
                        .iter()
                        .map(|s| (*s).to_string()),
                )
                .collect();
            &suppressed_extra
        } else {
            extra_flags
        };

        crate::compiler::compile_source(
            compiler_path,
            source,
            output,
            flags,
            effective_extra,
            &self.temp_dir,
            "ch32v",
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
        crate::compiler::build_cpp_flags(self.common_flags(), &self.mcu_config)
    }

    fn build_unflags(&self) -> &[String] {
        &self.build_unflags
    }

    /// Mirror the per-source suppression logic in [`Self::compile_one`] so the
    /// rebuild fingerprint changes whenever the third-party flag set changes.
    /// Otherwise an existing object compiled with the old flags would be
    /// considered up-to-date after the suppressions move or expand.
    fn rebuild_signature(&self, source: &Path, extra_flags: &[String]) -> String {
        let ext = source
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let (compiler_path, flags) = match ext.as_str() {
            "c" | "s" => (self.gcc_path(), self.c_flags()),
            _ => (self.gxx_path(), self.cpp_flags()),
        };
        let extra_owned: Vec<String> = if self.is_framework_source(source) {
            extra_flags
                .iter()
                .cloned()
                .chain(
                    framework_suppression_flags()
                        .iter()
                        .map(|s| (*s).to_string()),
                )
                .collect()
        } else {
            extra_flags.to_vec()
        };
        crate::compiler::build_rebuild_signature(compiler_path, &flags, &[], &extra_owned)
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

    #[test]
    fn test_is_framework_source_without_root() {
        let compiler = test_compiler();
        assert!(!compiler.is_framework_source(Path::new("/anywhere/foo.cpp")));
    }

    #[test]
    fn test_is_framework_source_with_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let framework_root = tmp.path().join("openwch-core");
        let core_dir = framework_root.join("cores/arduino/ch32");
        std::fs::create_dir_all(&core_dir).unwrap();
        let core_src = core_dir.join("analog.cpp");
        std::fs::write(&core_src, "// stub").unwrap();
        let user_dir = tmp.path().join("project/src");
        std::fs::create_dir_all(&user_dir).unwrap();
        let user_src = user_dir.join("main.cpp");
        std::fs::write(&user_src, "// stub").unwrap();

        let compiler = test_compiler().with_framework_root(framework_root);
        assert!(compiler.is_framework_source(&core_src));
        assert!(!compiler.is_framework_source(&user_src));
    }

    #[test]
    fn test_framework_suppression_flags_silence_everything() {
        // GCC's "excess elements in struct initializer" diagnostic has no
        // dedicated `-Wno-*` toggle, so the OpenWCH 1.0.4 core can only be
        // built cleanly with `-w`. Lock in the blanket-suppression policy so
        // a future "let's just disable specific warnings" patch trips this
        // test and re-reads the rationale in FastLED/fbuild#382.
        assert_eq!(framework_suppression_flags(), &["-w"]);
    }

    #[test]
    fn test_rebuild_signature_differs_for_framework_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        let framework_root = tmp.path().join("openwch-core");
        let core_dir = framework_root.join("cores/arduino/ch32");
        std::fs::create_dir_all(&core_dir).unwrap();
        let core_src = core_dir.join("analog.cpp");
        std::fs::write(&core_src, "// stub").unwrap();
        let user_src = tmp.path().join("main.cpp");
        std::fs::write(&user_src, "// stub").unwrap();

        let compiler = test_compiler().with_framework_root(framework_root);
        let core_sig = compiler.rebuild_signature(&core_src, &[]);
        let user_sig = compiler.rebuild_signature(&user_src, &[]);
        assert_ne!(
            core_sig, user_sig,
            "framework signature must include the suppression flags"
        );
    }
}
