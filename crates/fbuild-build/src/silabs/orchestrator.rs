//! Silicon Labs build orchestrator.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};
use fbuild_core::{Platform, Result};

use super::{SilabsCompiler, SilabsLinker};

/// Silicon Labs platform build orchestrator.
pub struct SilabsOrchestrator;

impl BuildOrchestrator for SilabsOrchestrator {
    fn platform(&self) -> Platform {
        Platform::SiliconLabs
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        let mut ctx = pipeline::BuildContext::new(params)?;

        let toolchain = fbuild_packages::toolchain::ArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("arm-gcc toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        );

        let framework = fbuild_packages::library::SilabsCores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
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
        pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &[],
            Some(&lib_env),
            TargetArchitecture::Arm,
            "Silicon Labs",
            start,
        )
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
