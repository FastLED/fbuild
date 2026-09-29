//! Compile local libraries from the project's `lib/` directory (PlatformIO convention).

use std::path::{Path, PathBuf};

use fbuild_core::Result;

use super::super::esp32_compiler::Esp32Compiler;
use crate::compiler::Compiler as _;
use crate::flag_overlay::{LanguageExtraFlags, apply_overlay_flags};

/// Compile each selected `lib/` library as an archive, appended to
/// `library_archives`. `libraries` comes from
/// [`crate::framework_libs::select_local_libraries`], so a library nothing
/// includes is never compiled (FastLED/fbuild#1410).
#[allow(clippy::too_many_arguments)]
pub(super) async fn compile_local_libraries(
    libraries: &[fbuild_packages::library::FrameworkLibrary],
    build_dir: &Path,
    compiler: &Esp32Compiler,
    toolchain: &fbuild_packages::toolchain::Esp32Toolchain,
    include_dirs: &[PathBuf],
    src_overlay: &LanguageExtraFlags,
    jobs: usize,
    verbose: bool,
    compiler_cache: Option<&Path>,
    library_archives: &mut Vec<PathBuf>,
) -> Result<()> {
    use fbuild_packages::Toolchain;

    for library in libraries {
        let lib_name = library.name.clone();
        let lib_sources = &library.source_files;
        if lib_sources.is_empty() {
            continue;
        }

        let lib_build_dir = build_dir.join("lib").join(&lib_name);
        tracing::info!(
            "compiling local library '{}': {} source files",
            lib_name,
            lib_sources.len()
        );

        // Use gcc-ar for LTO archives so the linker-plugin index is written.
        let local_ar_path = toolchain.get_ar_path();
        let local_gcc_ar_path = toolchain.get_gcc_ar_path();
        let local_c_flags = apply_overlay_flags(&compiler.c_flags(), src_overlay, "dummy.c");
        let local_cpp_flags = apply_overlay_flags(&compiler.cpp_flags(), src_overlay, "dummy.cpp");
        let local_lib_ar_path = crate::pipeline::pick_archiver(
            &local_ar_path,
            &local_gcc_ar_path,
            &local_c_flags,
            &local_cpp_flags,
        );
        match fbuild_packages::library::library_compiler::compile_library_with_jobs(
            &lib_name,
            lib_sources,
            include_dirs,
            &toolchain.get_gcc_path(),
            &toolchain.get_gxx_path(),
            local_lib_ar_path,
            &local_c_flags,
            &local_cpp_flags,
            &lib_build_dir,
            verbose,
            jobs,
            compiler_cache,
            None,
            None,
        )
        .await
        {
            Ok(Some(archive)) => {
                library_archives.push(archive);
            }
            Ok(None) => {} // header-only
            Err(e) => {
                return Err(fbuild_core::FbuildError::BuildFailed(format!(
                    "local library '{}' failed to compile: {}",
                    lib_name, e
                )));
            }
        }
    }
    Ok(())
}
