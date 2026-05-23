//! Compile framework built-in libraries (WiFi, FS, SPIFFS, Network, etc.)
//! shipped under `framework/libraries/<lib>/src/`. Linker `--gc-sections`
//! strips unused code, so we err on the side of compiling everything.

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::Result;
use fbuild_packages::Framework;

use super::super::esp32_compiler::Esp32Compiler;
use super::super::mcu_config::Esp32McuConfig;
use super::helpers::{
    apply_overlay_flags, framework_failure_marker, framework_signature,
    record_failed_framework_lib, should_skip_failed_framework_lib,
};
use crate::compiler::Compiler as _;
use crate::flag_overlay::LanguageExtraFlags;
use crate::BuildParams;

/// Compile every Arduino built-in library shipped with the ESP32 framework.
/// Library archives are appended to `library_archives`.
#[allow(clippy::too_many_arguments)]
pub(super) fn compile_framework_builtin_libs(
    params: &BuildParams,
    perf: &mut crate::perf_log::PerfTimer,
    framework: &fbuild_packages::library::Esp32Framework,
    toolchain: &fbuild_packages::toolchain::Esp32Toolchain,
    mcu_config: &Esp32McuConfig,
    board: &fbuild_config::BoardConfig,
    build_unflags: &[String],
    eh_frame_policy: crate::eh_frame_policy::EhFramePolicy,
    include_dirs: &[PathBuf],
    user_overlay: &LanguageExtraFlags,
    build_dir: &Path,
    compiler_cache: Option<&Path>,
    library_archives: &mut Vec<PathBuf>,
) -> Result<()> {
    use fbuild_packages::Toolchain;

    let fw_libs_started = Instant::now();
    perf.checkpoint("fw-libs-start");
    let builtin_libs_dir = framework.get_libraries_dir();
    if !builtin_libs_dir.is_dir() {
        perf.record("fw-libs", fw_libs_started.elapsed());
        perf.checkpoint("fw-libs-finish");
        return Ok(());
    }

    let fw_libs_build_dir = build_dir.join("fw_libs");
    std::fs::create_dir_all(&fw_libs_build_dir)?;

    // Build set of already-compiled library names
    let already_compiled: std::collections::HashSet<String> = library_archives
        .iter()
        .filter_map(|p| p.file_stem())
        .filter_map(|s| s.to_str())
        .filter_map(|s| s.strip_prefix("lib"))
        .map(|s| s.to_string())
        .collect();

    // Get compiler flags for framework library compilation
    let mut fw_defines = board.get_defines();
    fw_defines.extend(mcu_config.defines_map());

    let fw_compiler = Esp32Compiler::with_temp_dir(
        toolchain.get_gcc_path(),
        toolchain.get_gxx_path(),
        mcu_config.clone(),
        &board.f_cpu,
        fw_defines,
        include_dirs.to_vec(),
        params.profile,
        params.verbose,
        build_dir.join("tmp"),
    )
    .with_build_unflags(build_unflags.to_vec())
    .with_eh_frame_policy(eh_frame_policy);
    let fw_c_flags = apply_overlay_flags(&fw_compiler.c_flags(), user_overlay, "dummy.c");
    let fw_cpp_flags = apply_overlay_flags(&fw_compiler.cpp_flags(), user_overlay, "dummy.cpp");
    let fw_signature = framework_signature(include_dirs, &fw_c_flags, &fw_cpp_flags);

    let mut fw_lib_count = 0;
    let mut fw_lib_seen = 0;
    if let Ok(entries) = std::fs::read_dir(&builtin_libs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let lib_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if lib_name.starts_with('.') || already_compiled.contains(&lib_name) {
                continue;
            }

            let lib_src = path.join("src");
            if !lib_src.is_dir() {
                continue;
            }

            fw_lib_seen += 1;

            // Check if archive already exists
            let archive_path = fw_libs_build_dir.join(format!("lib{}.a", lib_name));
            if archive_path.exists() {
                if perf.is_active() {
                    perf.checkpoint(format!(
                        "fw-lib-cache-hit name={} index={}",
                        lib_name, fw_lib_seen
                    ));
                }
                library_archives.push(archive_path);
                fw_lib_count += 1;
                continue;
            }

            // Collect source files
            let lib_info =
                fbuild_packages::library::library_info::InstalledLibrary::new(&path, &lib_name);
            let sources = lib_info.get_source_files();
            if sources.is_empty() {
                continue;
            }
            let failure_marker = framework_failure_marker(&fw_libs_build_dir, &lib_name);
            if should_skip_failed_framework_lib(&failure_marker, &fw_signature, &sources)? {
                if perf.is_active() {
                    perf.checkpoint(format!(
                        "fw-lib-skip-failed name={} index={} sources={}",
                        lib_name,
                        fw_lib_seen,
                        sources.len()
                    ));
                }
                tracing::debug!(
                    "skipping previously failed framework library '{}'",
                    lib_name
                );
                continue;
            }

            let fw_jobs = crate::parallel::effective_jobs(params.jobs);
            if perf.is_active() {
                perf.checkpoint(format!(
                    "fw-lib-compile-start name={} index={} sources={} jobs={}",
                    lib_name,
                    fw_lib_seen,
                    sources.len(),
                    fw_jobs
                ));
            }
            // Use gcc-ar for LTO archives so the linker-plugin index is written.
            let fw_ar_path = toolchain.get_ar_path();
            let fw_gcc_ar_path = toolchain.get_gcc_ar_path();
            let fw_lib_ar_path = crate::pipeline::pick_archiver(
                &fw_ar_path,
                &fw_gcc_ar_path,
                &fw_c_flags,
                &fw_cpp_flags,
            );
            match fbuild_packages::library::library_compiler::compile_library_with_jobs(
                &lib_name,
                &sources,
                include_dirs,
                &toolchain.get_gcc_path(),
                &toolchain.get_gxx_path(),
                fw_lib_ar_path,
                &fw_c_flags,
                &fw_cpp_flags,
                &fw_libs_build_dir,
                params.verbose,
                fw_jobs,
                compiler_cache,
            ) {
                Ok(Some(archive)) => {
                    let _ = std::fs::remove_file(&failure_marker);
                    library_archives.push(archive);
                    fw_lib_count += 1;
                    if perf.is_active() {
                        perf.checkpoint(format!(
                            "fw-lib-compile-finish name={} index={} count={}",
                            lib_name, fw_lib_seen, fw_lib_count
                        ));
                    }
                }
                Ok(None) => {
                    if perf.is_active() {
                        perf.checkpoint(format!(
                            "fw-lib-header-only name={} index={}",
                            lib_name, fw_lib_seen
                        ));
                    }
                }
                Err(e) => {
                    // Non-fatal: some framework libs may fail to compile
                    // (e.g., platform-specific ones). The linker will report
                    // if any actually-needed symbols are missing.
                    if perf.is_active() {
                        perf.checkpoint(format!(
                            "fw-lib-compile-error name={} index={}",
                            lib_name, fw_lib_seen
                        ));
                    }
                    tracing::debug!("framework library {} failed to compile: {}", lib_name, e);
                    record_failed_framework_lib(&failure_marker, &fw_signature, &e.to_string());
                }
            }
        }
    }

    if fw_lib_count > 0 {
        tracing::info!("compiled {} framework built-in libraries", fw_lib_count);
    }
    perf.record("fw-libs", fw_libs_started.elapsed());
    perf.checkpoint("fw-libs-finish");
    Ok(())
}
