//! Apollo3 build orchestrator â€” wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (SparkFun_RedBoard_Artemis_ATP, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure Apollo3 cores (SparkFun Arduino Apollo3 core)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Parse mbed response files for flags/defines/includes
//! 8. Compile core + variant sources
//! 9. Compile sketch sources
//! 10. Link (with SVL linker script + libmbed-os.a) + convert to binary

use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compile_database::TargetArchitecture;
use crate::generic_arm::{ArmCompiler, ArmLinker};
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

/// Apollo3 platform build orchestrator.
pub struct Apollo3Orchestrator;

#[async_trait::async_trait]
impl BuildOrchestrator for Apollo3Orchestrator {
    fn platform(&self) -> Platform {
        Platform::Apollo3
    }

    async fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params).await?;

        // Compute eh_frame strip policy once per build (FastLED/fbuild#244).
        let eh_frame_policy =
            crate::eh_frame_policy_compute::compute_eh_frame_policy(&ctx, params.profile, None);

        // 3. Ensure ARM GCC 8 toolchain (Apollo3/mbed-os requires GCC 8)
        let toolchain = fbuild_packages::toolchain::ArmGcc8Toolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain).await?;
        tracing::info!("arm-gcc8 toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        )
        .await;

        // 4. Ensure Apollo3 cores (SparkFun Arduino Apollo3 core)
        let framework = fbuild_packages::library::Apollo3Cores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework).await?;
        tracing::info!("Apollo3 cores at {}", framework_dir.display());

        // 5. Scan sources (core + variant)
        let core_dir = framework.get_core_dir(&ctx.board.core);
        let variant_dir = framework.get_variant_dir(&ctx.board.variant);

        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let sources = scanner.scan_all_filtered(
            Some(&core_dir),
            Some(&variant_dir),
            ctx.source_filter.as_deref(),
        )?;

        let variant_config_dir = variant_dir.join("config");

        // `scan_all()` already recurses through the Apollo3 core and variant
        // trees. Re-scanning subdirectories here duplicates objects at link time.
        let mbed_bridge_dir = core_dir.join("mbed-bridge");

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 6. Build MCU config + merge defines from mbed response files
        let mcu_config =
            super::mcu_config::get_apollo3_config_for_mcu(&ctx.board.mcu.to_lowercase())?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());

        // Parse mbed symbol defines from variant response files
        let ld_symbols = framework.read_mbed_response_file(&ctx.board.variant, ".ld-symbols");
        for token in ld_symbols.split_whitespace() {
            if let Some(def) = token.strip_prefix("-D") {
                if let Some((key, val)) = def.split_once('=') {
                    defines.insert(key.to_string(), val.to_string());
                } else {
                    defines.insert(def.to_string(), "1".to_string());
                }
            }
        }

        // 7. Build include dirs
        let mut include_dirs = vec![core_dir.clone(), variant_dir.clone()];
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);

        // Note: Do NOT add toolchain sysroot includes for Apollo3.
        // GCC 9 handles its own internal multilib include paths; adding
        // generic -I paths breaks bits/c++config.h resolution.

        // mbed-bridge includes
        if mbed_bridge_dir.exists() {
            include_dirs.push(mbed_bridge_dir.clone());
            let core_api = mbed_bridge_dir.join("core-api");
            if core_api.exists() {
                include_dirs.push(core_api.clone());
                let api_dir = core_api.join("api");
                if api_dir.exists() {
                    include_dirs.push(api_dir);
                }
                let deprecated = core_api.join("api").join("deprecated");
                if deprecated.exists() {
                    include_dirs.push(deprecated);
                }
            }
        }

        // Variant config includes
        if variant_config_dir.exists() {
            include_dirs.push(variant_config_dir);
        }

        // Parse mbed .includes response file for additional include paths
        let mbed_includes = framework.read_mbed_response_file(&ctx.board.variant, ".includes");
        let core_prefix = framework.get_core_dir(""); // cores/ parent dir
        for token in mbed_includes.lines() {
            let token = token.trim().trim_matches('"');
            // Response file uses -iwithprefixbefore which prepends the -iprefix path
            // The -iprefix is set to {runtime.platform.path}/cores/ in platform.txt
            if let Some(relative) = token.strip_prefix("-iwithprefixbefore") {
                let abs_path = core_prefix.join(relative);
                if abs_path.exists() {
                    include_dirs.push(abs_path);
                }
            } else if let Some(path) = token.strip_prefix("-I") {
                let p = std::path::PathBuf::from(path);
                if p.exists() {
                    include_dirs.push(p);
                }
            }
        }

        // Add mbed_config.h via -include flag (handled as a define)
        let mbed_config_h = framework.get_mbed_config_h(&ctx.board.variant);
        let sdk_h = core_dir.join("sdk").join("ArduinoSDK.h");

        // `mbed_config.h` is required for both C and C++, but `ArduinoSDK.h`
        // pulls in C++ headers like `<chrono>` and breaks plain C compilation.
        let mut extra_common_flags: Vec<String> = Vec::new();
        let mut extra_cpp_flags: Vec<String> = Vec::new();
        if mbed_config_h.exists() {
            extra_common_flags.push("-include".to_string());
            extra_common_flags.push(mbed_config_h.to_string_lossy().to_string());
        }
        if sdk_h.exists() {
            extra_cpp_flags.push("-include".to_string());
            extra_cpp_flags.push(sdk_h.to_string_lossy().to_string());
        }

        // Merge the response-file includes into the MCU config.
        let mut augmented_config = mcu_config.clone();
        augmented_config
            .compiler_flags
            .common
            .extend(extra_common_flags);
        augmented_config.compiler_flags.cxx.extend(extra_cpp_flags);

        let compiler = ArmCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.mcu,
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            augmented_config.clone(),
            params.profile,
            params.verbose,
        )
        .with_build_unflags(ctx.build_unflags.clone())
        .with_eh_frame_policy(eh_frame_policy);

        // 8. Create linker
        let linker_script = framework.get_linker_script();

        // Parse extra linker flags from mbed response file
        let mbed_ld_flags = framework.read_mbed_response_file(&ctx.board.variant, ".ld-flags");
        let mut augmented_linker_config = augmented_config;
        for token in mbed_ld_flags.split_whitespace() {
            // Skip -D defines (already handled) and flags already in config
            if token.starts_with("-D") {
                continue;
            }
            if !augmented_linker_config
                .linker_flags
                .contains(&token.to_string())
            {
                augmented_linker_config.linker_flags.push(token.to_string());
            }
        }

        // Add libmbed-os.a to linker libs
        let mbed_lib = framework.get_mbed_lib(&ctx.board.variant);
        if mbed_lib.exists() {
            // Wrap in --whole-archive so all symbols are included
            augmented_linker_config
                .linker_libs
                .insert(0, "-Wl,--no-whole-archive".to_string());
            augmented_linker_config
                .linker_libs
                .insert(0, mbed_lib.to_string_lossy().to_string());
            augmented_linker_config
                .linker_libs
                .insert(0, "-Wl,--whole-archive".to_string());
        }

        let linker = ArmLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script,
            augmented_linker_config,
            params.profile,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        );

        // 9. Build LibraryBuildEnv for project-as-library compilation
        let gcc_path = toolchain.get_gcc_path();
        let gxx_path = toolchain.get_gxx_path();
        let ar_path = toolchain.get_ar_path();
        let gcc_ar_path = toolchain.get_gcc_ar_path();
        let c_flags = crate::compiler::Compiler::c_flags(&compiler);
        let cpp_flags = crate::compiler::Compiler::cpp_flags(&compiler);
        // Use gcc-ar for LTO archives so the linker-plugin index is written.
        let lib_ar_path = pipeline::pick_archiver(&ar_path, &gcc_ar_path, &c_flags, &cpp_flags);
        let lib_env = pipeline::LibraryBuildEnv {
            gcc_path: &gcc_path,
            gxx_path: &gxx_path,
            ar_path: lib_ar_path,
            c_flags: &c_flags,
            cpp_flags: &cpp_flags,
            include_dirs: &include_dirs,
            verbose: params.verbose,
            jobs: crate::parallel::effective_jobs(params.jobs),
            compiler_cache: None,
        };

        // 10. Run shared sequential build pipeline
        pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &[],
            Some(&lib_env),
            TargetArchitecture::Arm,
            "APOLLO3",
            start,
        )
        .await
    }
}

/// Create an Apollo3 orchestrator.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Apollo3Orchestrator)
}

/// Check if a project is configured for Apollo3.
pub fn is_apollo3_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::Apollo3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apollo3_orchestrator_platform() {
        let orch = Apollo3Orchestrator;
        assert_eq!(orch.platform(), Platform::Apollo3);
    }
}
