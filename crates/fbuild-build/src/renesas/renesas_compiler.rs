//! Renesas RA ARM Cortex-M4 compiler implementation.
//!
//! Compiles C/C++ source files using arm-none-eabi-gcc/g++ with appropriate
//! flags for Renesas RA boards (ARM Cortex-M4, hardware FPU).
//!
//! ## FSP warning scope (FastLED/fbuild#404)
//!
//! Four critical-bug-class C diagnostics -- `return-mismatch`,
//! `implicit-function-declaration`, `int-conversion`, and
//! `incompatible-pointer-types` -- used to be demoted to warnings
//! workspace-wide via `ra4m1.json`'s `compiler_flags.c` array. That hid
//! real undefined behaviour in FastLED + user-sketch code. The demotion
//! is now scoped to ArduinoCore-renesas vendor C sources only via
//! [`RenesasCompiler::with_framework_root`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::path::NormalizedPath;
use fbuild_core::{BuildProfile, Result};

use super::mcu_config::RenesasMcuConfig;
use crate::compiler::{CompileResult, Compiler, CompilerBase};

/// Renesas-specific compiler using arm-none-eabi-gcc and arm-none-eabi-g++.
pub struct RenesasCompiler {
    pub base: CompilerBase,
    gcc_path: PathBuf,
    gxx_path: PathBuf,
    mcu_config: RenesasMcuConfig,
    profile: BuildProfile,
    temp_dir: PathBuf,
    /// PlatformIO `build_unflags`. See FastLED/fbuild#37.
    build_unflags: Vec<String>,
    /// Root of the ArduinoCore-renesas install. C sources rooted under this
    /// path get the four upstream FSP C-bug-class diagnostics demoted from
    /// errors to warnings -- those classes (`return-mismatch`,
    /// `implicit-function-declaration`, `int-conversion`,
    /// `incompatible-pointer-types`) are pre-existing in the pinned 1.2.2
    /// release and cannot be fixed from here. FastLED + user sketch sources
    /// stay under the stricter default `-Werror=` posture so those same bug
    /// classes still fail the build when introduced in user code. See
    /// FastLED/fbuild#404.
    framework_root: Option<PathBuf>,
}

impl RenesasCompiler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        gxx_path: PathBuf,
        mcu: &str,
        f_cpu: &str,
        defines: HashMap<String, String>,
        include_dirs: Vec<PathBuf>,
        mcu_config: RenesasMcuConfig,
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
            framework_root: None,
        }
    }

    /// Attach PlatformIO `build_unflags`. See FastLED/fbuild#37.
    pub fn with_build_unflags(mut self, build_unflags: Vec<String>) -> Self {
        self.build_unflags = build_unflags;
        self
    }

    /// Scope FSP warning demotions to sources under `root`.
    ///
    /// Used to keep the four upstream-FSP C diagnostics
    /// (`return-mismatch`, `implicit-function-declaration`, `int-conversion`,
    /// `incompatible-pointer-types`) downgraded to warnings for vendor code
    /// while still failing the build when FastLED or user-sketch sources
    /// introduce one of the same bug classes. See FastLED/fbuild#404.
    pub fn with_framework_root(mut self, root: PathBuf) -> Self {
        self.framework_root = Some(root);
        self
    }

    /// Build the common ARM Cortex-M4 compiler flags.
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

/// Flags appended when compiling third-party Renesas FSP / ArduinoCore-renesas
/// C sources.
///
/// The pinned ArduinoCore-renesas 1.2.2 release ships FSP C code that GCC
/// rejects under the default `-Werror=` posture for four specific bug-class
/// diagnostics. We can't fix the upstream code from here, so we demote those
/// four -- and only those four -- back to warnings for vendor sources. The
/// same diagnostics still fail the build when introduced in FastLED or user
/// sketch code, which is the whole point: these classes (return-type
/// mismatch, implicit function decl, int/pointer conversion, incompatible
/// pointer types) catch real undefined behaviour and the audit in
/// FastLED/fbuild#404 found they were previously silenced workspace-wide.
///
/// Scope: framework C sources only (`*.c`). The relevant diagnostics don't
/// apply to C++ and the upstream FSP code is C, so this list is
/// intentionally C-only. See FastLED/fbuild#404.
fn framework_c_suppression_flags() -> &'static [&'static str] {
    &[
        "-Wno-error=return-mismatch",
        "-Wno-error=implicit-function-declaration",
        "-Wno-error=int-conversion",
        "-Wno-error=incompatible-pointer-types",
    ]
}

/// `true` when `source` is a C source file (`.c`). Assembly and C++ sources
/// don't get the FSP demotion set.
fn is_c_source(source: &Path) -> bool {
    source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("c"))
        .unwrap_or(false)
}

#[async_trait::async_trait]
impl Compiler for RenesasCompiler {
    async fn compile_one(
        &self,
        compiler_path: &Path,
        source: &Path,
        output: &Path,
        flags: &[String],
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        // Append FSP warning demotions after user-provided flags so they
        // override any earlier `-Werror=` toggles. Scoped strictly to C
        // sources rooted in the ArduinoCore-renesas install; sketch sources
        // and project libraries continue to see the full default `-Werror=`
        // posture for these four bug classes. See FastLED/fbuild#404.
        let suppressed_extra: Vec<String>;
        let effective_extra: &[String] = if is_c_source(source) && self.is_framework_source(source)
        {
            suppressed_extra = extra_flags
                .iter()
                .cloned()
                .chain(
                    framework_c_suppression_flags()
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
            "renesas",
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

    /// Mirror the per-source suppression logic in [`Self::compile_one`] so
    /// the rebuild fingerprint changes whenever the FSP demotion set moves
    /// or expands. Without this, an object compiled with the old flags would
    /// be considered up-to-date after the suppressions change. See
    /// FastLED/fbuild#404.
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
        let extra_owned: Vec<String> = if is_c_source(source) && self.is_framework_source(source) {
            extra_flags
                .iter()
                .cloned()
                .chain(
                    framework_c_suppression_flags()
                        .iter()
                        .map(|s| (*s).to_string()),
                )
                .collect()
        } else {
            extra_flags.to_vec()
        };
        // build_unflags stripped inside build_rebuild_signature (shared core),
        // matching compile_c/compile_cpp on the write side (FastLED/fbuild#970).
        crate::compiler::build_rebuild_signature(
            compiler_path,
            &flags,
            &[],
            &extra_owned,
            self.build_unflags(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renesas::mcu_config::get_renesas_config_for_mcu;

    fn test_compiler() -> RenesasCompiler {
        let mut defines = HashMap::new();
        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        defines.insert("F_CPU".to_string(), "48000000L".to_string());
        defines.insert("ARDUINO".to_string(), "10808".to_string());

        RenesasCompiler::new(
            PathBuf::from("/usr/bin/arm-none-eabi-gcc"),
            PathBuf::from("/usr/bin/arm-none-eabi-g++"),
            "ra4m1",
            "48000000L",
            defines,
            vec![PathBuf::from("/renesas/cores")],
            get_renesas_config_for_mcu("ra4m1").unwrap(),
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
        assert!(flags.contains(&"-fno-threadsafe-statics".to_string()));
    }

    /// Regression guard for FastLED/fbuild#404: the four FSP-bug-class
    /// `-Wno-error=` flags MUST NOT be in the workspace-wide C flag set. If
    /// they leak back into `ra4m1.json`'s `compiler_flags.c` array, every C
    /// translation unit -- including the user sketch -- will silently demote
    /// these errors to warnings and the original audit finding returns.
    #[test]
    fn test_c_flags_do_not_contain_fsp_suppressions_globally() {
        let compiler = test_compiler();
        let flags = compiler.c_flags();
        for sup in framework_c_suppression_flags() {
            assert!(
                !flags.iter().any(|f| f == *sup),
                "{} must not be in the workspace-wide C flag set; \
                 it belongs to the per-FSP-source scope only \
                 (see FastLED/fbuild#404)",
                sup
            );
        }
    }

    #[test]
    fn test_is_framework_source_without_root() {
        let compiler = test_compiler();
        assert!(!compiler.is_framework_source(Path::new("/anywhere/foo.c")));
    }

    #[test]
    fn test_is_framework_source_with_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let framework_root = tmp.path().join("ArduinoCore-renesas");
        let fsp_dir = framework_root.join("variants/UNOWIFIR4/includes/ra/fsp/src");
        std::fs::create_dir_all(&fsp_dir).unwrap();
        let fsp_src = fsp_dir.join("r_ioport.c");
        std::fs::write(&fsp_src, "// stub").unwrap();
        let user_dir = tmp.path().join("project/src");
        std::fs::create_dir_all(&user_dir).unwrap();
        let user_src = user_dir.join("main.c");
        std::fs::write(&user_src, "// stub").unwrap();

        let compiler = test_compiler().with_framework_root(framework_root);
        assert!(compiler.is_framework_source(&fsp_src));
        assert!(!compiler.is_framework_source(&user_src));
    }

    /// Lock in the precise set of FSP warning demotions. Each one disables
    /// a real-undefined-behaviour diagnostic -- adding a new entry should
    /// make a reviewer re-read FastLED/fbuild#404 and explain why the new
    /// flag is safe for vendor code.
    #[test]
    fn test_framework_c_suppression_set_is_locked_in() {
        assert_eq!(
            framework_c_suppression_flags(),
            &[
                "-Wno-error=return-mismatch",
                "-Wno-error=implicit-function-declaration",
                "-Wno-error=int-conversion",
                "-Wno-error=incompatible-pointer-types",
            ]
        );
    }

    #[test]
    fn test_is_c_source_classification() {
        assert!(is_c_source(Path::new("foo.c")));
        assert!(is_c_source(Path::new("FOO.C")));
        assert!(!is_c_source(Path::new("foo.cpp")));
        assert!(!is_c_source(Path::new("foo.cc")));
        assert!(!is_c_source(Path::new("foo.S")));
        assert!(!is_c_source(Path::new("foo.s")));
        assert!(!is_c_source(Path::new("foo")));
    }

    /// FastLED/fbuild#404 acceptance: rebuild signature for an FSP C source
    /// must differ from a user-sketch C source, because the per-file flag
    /// set differs. If they collide, an old object compiled with the wrong
    /// flag set would be reused after this scoping change ships.
    #[test]
    fn test_rebuild_signature_differs_for_framework_c_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        let framework_root = tmp.path().join("ArduinoCore-renesas");
        let fsp_dir = framework_root.join("variants/UNOWIFIR4/includes/ra/fsp/src");
        std::fs::create_dir_all(&fsp_dir).unwrap();
        let fsp_src = fsp_dir.join("r_ioport.c");
        std::fs::write(&fsp_src, "// stub").unwrap();
        let user_src = tmp.path().join("main.c");
        std::fs::write(&user_src, "// stub").unwrap();

        let compiler = test_compiler().with_framework_root(framework_root);
        let fsp_sig = compiler.rebuild_signature(&fsp_src, &[]);
        let user_sig = compiler.rebuild_signature(&user_src, &[]);
        assert_ne!(
            fsp_sig, user_sig,
            "FSP C signature must include the four -Wno-error= flags; \
             user-sketch C signature must not. See FastLED/fbuild#404."
        );
    }

    /// FastLED/fbuild#404 scope guard: a C++ source under the framework root
    /// MUST NOT receive the C-only FSP demotions, because the relevant
    /// diagnostics don't apply to C++ and the FSP upstream code is C. This
    /// keeps the suppression surface as small as possible.
    #[test]
    fn test_framework_cpp_source_does_not_get_c_suppressions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let framework_root = tmp.path().join("ArduinoCore-renesas");
        let core_dir = framework_root.join("cores/arduino");
        std::fs::create_dir_all(&core_dir).unwrap();
        let cpp_src = core_dir.join("Serial.cpp");
        std::fs::write(&cpp_src, "// stub").unwrap();
        let baseline_cpp = tmp.path().join("Serial.cpp");
        std::fs::write(&baseline_cpp, "// stub").unwrap();

        let compiler = test_compiler().with_framework_root(framework_root);
        let framework_cpp_sig = compiler.rebuild_signature(&cpp_src, &[]);
        let baseline_cpp_sig = compiler.rebuild_signature(&baseline_cpp, &[]);
        assert_eq!(
            framework_cpp_sig, baseline_cpp_sig,
            "framework C++ sources must not pick up the C-only FSP \
             suppressions (FastLED/fbuild#404)"
        );
    }
}
