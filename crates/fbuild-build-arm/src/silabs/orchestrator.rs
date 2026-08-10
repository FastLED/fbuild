//! Silicon Labs build orchestrator.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::build_fingerprint::{
    CoreFingerprintMetadata, FastPathCheckInputs, FastPathContract, FastPathPersistInputs,
    expected_fast_path_artifacts, stable_hash_json,
};
use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};
use fbuild_core::{Platform, Result};

use super::{SilabsCompiler, SilabsLinker};

/// Silicon Labs platform build orchestrator.
pub struct SilabsOrchestrator;

fn profile_label(profile: fbuild_core::BuildProfile) -> &'static str {
    match profile {
        fbuild_core::BuildProfile::Release => "release",
        fbuild_core::BuildProfile::Quick => "quick",
    }
}

#[async_trait::async_trait]
impl BuildOrchestrator for SilabsOrchestrator {
    fn platform(&self) -> Platform {
        Platform::SiliconLabs
    }

    async fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        let mut ctx = pipeline::BuildContext::new(params).await?;

        let toolchain = fbuild_packages::toolchain::ArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain).await?;
        tracing::info!("arm-gcc toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        )
        .await;

        // Honor `platform_packages` override (FastLED/fbuild#664, #681).
        let __ovr = ctx
            .config
            .get_env_config(&params.env_name)
            .ok()
            .and_then(|env| {
                crate::package_override::resolve_override(env, "framework-arduinosilabs")
            });
        let framework = match __ovr {
            Some(o) => fbuild_packages::library::SilabsCores::with_override(&params.project_dir, o),
            None => fbuild_packages::library::SilabsCores::new(&params.project_dir),
        };
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework).await?;
        tracing::info!("Silicon Labs cores at {}", framework_dir.display());

        let core_dir = framework.get_core_dir(&ctx.board.core);
        let variant_dir = framework.get_variant_dir(&ctx.board.variant);
        let protocol_stack = resolve_protocol_stack(&ctx, &params.env_name);
        let stack_dir = variant_dir.join(&protocol_stack);
        if protocol_stack != "noradio" {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "Silicon Labs protocol stack '{}' is not implemented yet; supported stack: noradio",
                protocol_stack
            )));
        }
        if !stack_dir.is_dir() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "Silicon Labs stack directory not found: {}",
                stack_dir.display()
            )));
        }

        let build_dir = &ctx.build_dir;
        let metadata_hash = stable_hash_json(&CoreFingerprintMetadata {
            version: crate::build_fingerprint::BUILD_FINGERPRINT_VERSION,
            env_name: params.env_name.clone(),
            profile: profile_label(params.profile).to_string(),
            board_name: ctx.board.name.clone(),
            board_mcu: ctx.board.mcu.clone(),
            board_define: ctx.board.board.clone(),
            board_core: ctx.board.core.clone(),
            board_f_cpu: ctx.board.f_cpu.clone(),
            board_extra_flags: ctx.board.extra_flags.clone(),
            board_ldscript: ctx.board.ldscript.clone(),
            board_variant: Some(ctx.board.variant.clone()),
            platform: "silabs".to_string(),
            max_flash: ctx.board.max_flash,
            max_ram: ctx.board.max_ram,
            eh_frame_policy: None,
            extra: Some(std::collections::BTreeMap::from([(
                "protocol_stack".to_string(),
                protocol_stack.clone(),
            )])),
        })?;
        let (fast_elf, [fast_bin], fast_compile_db) =
            expected_fast_path_artifacts(build_dir, &params.project_dir, ["firmware.bin"]);
        let fast_path = FastPathContract::for_project_outputs(
            build_dir,
            &params.project_dir,
            [fast_elf.clone(), fast_bin.clone(), fast_compile_db.clone()],
        );
        let compiler_cache: Option<fbuild_core::path::NormalizedPath> = None;

        if !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            let inputs = FastPathCheckInputs {
                metadata_hash: &metadata_hash,
                extra_artifact_ok: None,
                watch_set_cache: params.watch_set_cache.as_deref(),
                compiler_cache: compiler_cache.as_deref(),
            };
            if let Some(hit) = crate::build_fingerprint::fast_path_check(&fast_path, &inputs)? {
                let elapsed = start.elapsed().as_secs_f64();
                return Ok(crate::build_fingerprint::assemble_fast_path_result(
                    hit,
                    ctx.build_log,
                    crate::build_fingerprint::FastPathResultInputs {
                        platform_label: "Silicon Labs",
                        mcu: &ctx.board.mcu,
                        env_name: &params.env_name,
                        firmware_path: fast_bin,
                        elf_path: fast_elf,
                        compile_database_path: fast_compile_db,
                        elapsed,
                    },
                ));
            }
        }

        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let mut sources =
            scanner.scan_all_filtered(Some(&core_dir), None, ctx.source_filter.as_deref())?;
        sources.variant_sources = scan_variant_root_sources(&variant_dir);

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        let mcu_name = ctx.board.mcu.to_lowercase();
        let mcu_config = super::mcu_config::get_silabs_config_for_mcu(&mcu_name)?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        defines.extend(silabs_noradio_defines(&ctx.board.variant, &protocol_stack)?);
        defines.insert("ARDUINO_ARCH_SILABS".to_string(), "1".to_string());
        defines.insert("ARDUINO_THINGPLUSMATTER".to_string(), "1".to_string());
        defines.insert("ARDUINO_SILABS".to_string(), "\\\"2.2.0\\\"".to_string());

        let mut include_dirs = vec![core_dir.clone(), variant_dir.clone(), ctx.src_dir.clone()];
        let core_avr = core_dir.join("avr");
        if core_avr.is_dir() {
            include_dirs.push(core_avr);
        }
        let stack_include = stack_dir.join("include");
        if stack_include.is_dir() {
            include_dirs.push(stack_include);
        }
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        include_dirs.extend(toolchain.get_include_dirs());

        // 6a. Download `lib_deps` from the registry / remote URLs before
        // creating the compiler, so the downloaded library include directories
        // are available during compilation (FastLED/fbuild#1276).
        let lib_deps = ctx.config.get_lib_deps(&params.env_name)?;
        let lib_ignore = ctx
            .config
            .get_lib_ignore(&params.env_name)
            .unwrap_or_default();
        let lib_archives = if !lib_deps.is_empty() {
            let temp_compiler = SilabsCompiler::new(
                toolchain.get_gcc_path(),
                toolchain.get_gxx_path(),
                &ctx.board.mcu,
                &ctx.board.f_cpu,
                defines.clone(),
                include_dirs.clone(),
                mcu_config.clone(),
                params.profile,
                params.verbose,
            );
            pipeline::resolve_lib_deps(
                &lib_deps,
                &lib_ignore,
                &params.project_dir,
                &ctx.build_dir,
                &toolchain.get_gcc_path(),
                &toolchain.get_gxx_path(),
                &toolchain.get_ar_path(),
                &toolchain.get_gcc_ar_path(),
                &crate::compiler::Compiler::c_flags(&temp_compiler),
                &crate::compiler::Compiler::cpp_flags(&temp_compiler),
                &mut include_dirs,
                params.verbose,
                crate::parallel::effective_jobs(params.jobs),
                None,
            )
            .await?
        } else {
            Vec::new()
        };

        let compiler = SilabsCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.mcu,
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
        )
        .with_build_unflags(ctx.build_unflags.clone());

        let linker_script = stack_dir.join("linkerfile.ld");
        let gsdk = stack_dir.join("gsdk.a");
        let precompiled_gsdk = gsdk.is_file().then_some(gsdk);
        let precompiled_libs = ["libnvm3_CM33_gcc.a"]
            .into_iter()
            .map(|name| stack_dir.join(name))
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        let linker = SilabsLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script,
            precompiled_gsdk,
            precompiled_libs,
            mcu_config.clone(),
            params.profile,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        );

        let gcc_path = toolchain.get_gcc_path();
        let gxx_path = toolchain.get_gxx_path();
        let ar_path = toolchain.get_ar_path();
        let gcc_ar_path = toolchain.get_gcc_ar_path();
        let c_flags = crate::compiler::Compiler::c_flags(&compiler);
        let cpp_flags = crate::compiler::Compiler::cpp_flags(&compiler);
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
        let result = pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &lib_archives,
            Some(&lib_env),
            TargetArchitecture::Arm,
            "Silicon Labs",
            start,
        )
        .await?;

        if result.success
            && !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            crate::build_fingerprint::persist_fast_path_success(
                &fast_path,
                &FastPathPersistInputs {
                    metadata_hash: &metadata_hash,
                    size_info: result.size_info.clone(),
                    watch_set_cache: params.watch_set_cache.as_deref(),
                    compiler_cache: compiler_cache.as_deref(),
                },
            );
        }

        Ok(result)
    }
}

fn resolve_protocol_stack(ctx: &pipeline::BuildContext, env_name: &str) -> String {
    ctx.config
        .get_env_config(env_name)
        .ok()
        .and_then(|env| env.get("protocol_stack").cloned())
        .unwrap_or_else(|| "noradio".to_string())
}

fn scan_variant_root_sources(variant_dir: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    if let Ok(entries) = std::fs::read_dir(variant_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if matches!(ext.as_str(), "c" | "cc" | "cpp" | "s") {
                sources.push(path);
            }
        }
    }
    sources.sort();
    sources
}

fn silabs_noradio_defines(variant: &str, protocol_stack: &str) -> Result<HashMap<String, String>> {
    if variant != "thingplusmatter" || protocol_stack != "noradio" {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "unsupported Silicon Labs board configuration: variant='{}', protocol_stack='{}'",
            variant, protocol_stack
        )));
    }

    let mut defines = HashMap::new();
    defines.insert("NUM_LEDS".to_string(), "1".to_string());
    defines.insert("NUM_HW_SERIAL".to_string(), "2".to_string());
    defines.insert("NUM_HW_SPI".to_string(), "2".to_string());
    defines.insert("NUM_HW_I2C".to_string(), "1".to_string());
    defines.insert("NUM_DAC_HW".to_string(), "2".to_string());
    defines.insert(
        "ARDUINO_MAIN_TASK_STACK_SIZE".to_string(),
        "2048".to_string(),
    );
    defines.insert("MGM240PB32VNA".to_string(), "1".to_string());
    defines.insert("SL_APP_PROPERTIES".to_string(), "1".to_string());
    defines.insert(
        "HARDWARE_BOARD_DEFAULT_RF_BAND_2400".to_string(),
        "1".to_string(),
    );
    defines.insert(
        "HARDWARE_BOARD_SUPPORTS_1_RF_BAND".to_string(),
        "1".to_string(),
    );
    defines.insert(
        "HARDWARE_BOARD_SUPPORTS_RF_BAND_2400".to_string(),
        "1".to_string(),
    );
    defines.insert("SL_BOARD_NAME".to_string(), "\\\"BRD2704A\\\"".to_string());
    defines.insert("SL_BOARD_REV".to_string(), "\\\"A00\\\"".to_string());
    defines.insert(
        "configNUM_SDK_THREAD_LOCAL_STORAGE_POINTERS".to_string(),
        "2".to_string(),
    );
    defines.insert("SL_COMPONENT_CATALOG_PRESENT".to_string(), "1".to_string());
    defines.insert(
        "MBEDTLS_CONFIG_FILE".to_string(),
        "<sl_mbedtls_config.h>".to_string(),
    );
    defines.insert(
        "MBEDTLS_PSA_CRYPTO_CONFIG_FILE".to_string(),
        "<psa_crypto_config.h>".to_string(),
    );
    Ok(defines)
}

/// Create a Silicon Labs orchestrator.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(SilabsOrchestrator)
}

/// Check if a project is configured for Silicon Labs by reading its platformio.ini.
pub fn is_silabs_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::SiliconLabs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_silabs_orchestrator_platform() {
        let orch = SilabsOrchestrator;
        assert_eq!(orch.platform(), Platform::SiliconLabs);
    }
}
