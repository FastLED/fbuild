//! Compile local libraries from the project's `lib/` directory (PlatformIO convention).

use std::path::{Path, PathBuf};

use fbuild_core::Result;
use fbuild_packages::library::library_compiler::JobGate;

use super::super::esp32_compiler::Esp32Compiler;
use crate::compiler::Compiler as _;
use crate::flag_overlay::{LanguageExtraFlags, apply_overlay_flags};

/// Compile each selected `lib/` library as an archive. `libraries` comes from
/// [`crate::framework_libs::select_local_libraries`], so a library nothing
/// includes is never compiled (FastLED/fbuild#1410).
///
/// All libraries compile concurrently, each translation unit drawing a permit
/// from the build's shared `gate` (FastLED/fbuild#1559). Archives come back in
/// library order (the link order); the first failure in that order is the
/// error, reported once every library has finished.
#[allow(clippy::too_many_arguments)]
pub(super) async fn compile_local_libraries(
    libraries: &[fbuild_packages::library::FrameworkLibrary],
    build_dir: &Path,
    compiler: &Esp32Compiler,
    toolchain: &fbuild_packages::toolchain::Esp32Toolchain,
    include_dirs: &[PathBuf],
    src_overlay: &LanguageExtraFlags,
    gate: &JobGate,
    verbose: bool,
    compiler_cache: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    use fbuild_packages::Toolchain;

    let c_flags = apply_overlay_flags(&compiler.c_flags(), src_overlay, "dummy.c");
    let cpp_flags = apply_overlay_flags(&compiler.cpp_flags(), src_overlay, "dummy.cpp");
    // Use gcc-ar for LTO archives so the linker-plugin index is written.
    let ar_path = toolchain.get_ar_path();
    let gcc_ar_path = toolchain.get_gcc_ar_path();
    let archiver = crate::pipeline::pick_archiver(&ar_path, &gcc_ar_path, &c_flags, &cpp_flags);
    let gcc_path = toolchain.get_gcc_path();
    let gxx_path = toolchain.get_gxx_path();

    let compiles = libraries
        .iter()
        .filter(|library| !library.source_files.is_empty())
        .map(|library| {
            let lib_build_dir = build_dir.join("lib").join(&library.name);
            tracing::info!(
                "compiling local library '{}': {} source files",
                library.name,
                library.source_files.len()
            );
            let (c_flags, cpp_flags) = (&c_flags, &cpp_flags);
            let (gcc_path, gxx_path) = (&gcc_path, &gxx_path);
            async move {
                let result = fbuild_packages::library::library_compiler::compile_library_gated(
                    &library.name,
                    &library.source_files,
                    include_dirs,
                    gcc_path,
                    gxx_path,
                    archiver,
                    c_flags,
                    cpp_flags,
                    &lib_build_dir,
                    verbose,
                    gate,
                    compiler_cache,
                    None,
                    None,
                )
                .await;
                (library.name.as_str(), result)
            }
        });
    // join_all runs every compile to completion even if one fails.
    let outcomes = futures::future::join_all(compiles).await;

    let mut archives = Vec::new();
    for (lib_name, outcome) in outcomes {
        match outcome {
            Ok(Some(archive)) => archives.push(archive),
            Ok(None) => {} // header-only
            Err(e) => {
                return Err(fbuild_core::FbuildError::BuildFailed(format!(
                    "local library '{}' failed to compile: {}",
                    lib_name, e
                )));
            }
        }
    }
    Ok(archives)
}

/// The project's own `src/` compiled as a library archive when the project
/// root carries `library.json`/`library.properties` (e.g. FastLED building an
/// example); shared with the sequential pipeline via
/// [`crate::pipeline::compile_project_as_library`].
pub(super) struct ProjectLibrary {
    c_flags: Vec<String>,
    cpp_flags: Vec<String>,
    ar_path: PathBuf,
    /// `lib/*` names, so a collision with the project-as-library is detected.
    existing_lib_names: std::collections::HashSet<String>,
    jobs: usize,
}

impl ProjectLibrary {
    /// Flags as the sketch sees them: SDK defines + user flags, so the
    /// archive matches what sketch sources compile against.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare(
        params: &crate::BuildParams,
        toolchain: &fbuild_packages::toolchain::Esp32Toolchain,
        mcu_config: &super::super::mcu_config::Esp32McuConfig,
        board: &fbuild_config::BoardConfig,
        build_unflags: &[String],
        eh_frame_policy: crate::eh_frame_policy::EhFramePolicy,
        include_dirs: &[PathBuf],
        src_overlay: &LanguageExtraFlags,
        build_dir: &Path,
    ) -> Self {
        use fbuild_packages::Toolchain;

        let mut defines = board.get_defines();
        defines.extend(mcu_config.defines_map());
        let compiler = Esp32Compiler::with_temp_dir(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            mcu_config.clone(),
            &board.f_cpu,
            defines,
            include_dirs.to_vec(),
            params.profile,
            params.verbose,
            build_dir.join("tmp"),
        )
        .with_build_unflags(build_unflags.to_vec())
        .with_eh_frame_policy(eh_frame_policy);
        let c_flags = apply_overlay_flags(&compiler.c_flags(), src_overlay, "dummy.c");
        let cpp_flags = apply_overlay_flags(&compiler.cpp_flags(), src_overlay, "dummy.cpp");

        let mut existing_lib_names = std::collections::HashSet::new();
        if let Ok(entries) = std::fs::read_dir(params.project_dir.join("lib")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        existing_lib_names.insert(name.to_lowercase());
                    }
                }
            }
        }

        // Use gcc-ar for LTO archives so the linker-plugin index is written.
        let ar_path = crate::pipeline::pick_archiver(
            &toolchain.get_ar_path(),
            &toolchain.get_gcc_ar_path(),
            &c_flags,
            &cpp_flags,
        )
        .to_path_buf();
        Self {
            c_flags,
            cpp_flags,
            ar_path,
            existing_lib_names,
            jobs: crate::parallel::effective_jobs(params.jobs),
        }
    }

    /// The archive name (`lib{name}.a`) this produces, if any, before compiling.
    pub(super) fn name(&self, project_dir: &Path, src_dir: &Path) -> Option<String> {
        crate::pipeline::project_library_name(project_dir, src_dir, &self.existing_lib_names)
    }

    /// Compile on the build's shared `gate`.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn compile(
        &self,
        project_dir: &Path,
        src_dir: &Path,
        build_dir: &Path,
        toolchain: &fbuild_packages::toolchain::Esp32Toolchain,
        include_dirs: &[PathBuf],
        verbose: bool,
        compiler_cache: Option<&Path>,
        gate: &JobGate,
    ) -> Result<Option<PathBuf>> {
        use fbuild_packages::Toolchain;

        let gcc_path = toolchain.get_gcc_path();
        let gxx_path = toolchain.get_gxx_path();
        let env = crate::pipeline::LibraryBuildEnv {
            gcc_path: &gcc_path,
            gxx_path: &gxx_path,
            ar_path: &self.ar_path,
            c_flags: &self.c_flags,
            cpp_flags: &self.cpp_flags,
            include_dirs,
            verbose,
            jobs: self.jobs,
            compiler_cache,
        };
        crate::pipeline::compile_project_as_library(
            project_dir,
            src_dir,
            build_dir,
            &env,
            &self.existing_lib_names,
            gate,
        )
        .await
    }
}
