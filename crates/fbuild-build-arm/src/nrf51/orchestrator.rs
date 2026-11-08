//! nRF51 build orchestrator — sandeepmistry arduino-nRF5 on Cortex-M0.
//!
//! Mirrors PlatformIO's `platform-nordicnrf51` `builder/frameworks/arduino/nrf5.py`:
//! the whole `cores/<core>` tree (including the bundled Nordic SDK startup
//! and `system_nrf51.c`) plus the board variant are compiled as core
//! sources, and the board's `build.arduino.ldscript` (e.g. `nrf51_xxaa.ld`,
//! no SoftDevice) is linked from `SDK/components/toolchain/gcc`. The shared
//! `generic_arm` compiler/linker pipeline does the rest.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Platform, Result};

use crate::build_fingerprint::{
    BUILD_FINGERPRINT_VERSION, CoreFingerprintMetadata, FastPathCheckInputs, FastPathContract,
    FastPathPersistInputs, expected_fast_path_artifacts, stable_hash_with_build_config,
};
use crate::compile_database::TargetArchitecture;
use crate::generic_arm::{ArmCompiler, ArmLinker};
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::mcu_config::get_nrf51_config_for_mcu;

const FRAMEWORK_PACKAGE: &str = "framework-arduinonordicnrf5";
const DEFAULT_LDSCRIPT: &str = "nrf51_xxaa.ld";
/// Low-frequency clock selectors understood by arduino-nRF5. PlatformIO
/// defaults to the crystal (`USE_LFXO`) when the board selects none.
const LF_CLOCK_DEFINES: [&str; 3] = ["USE_LFXO", "USE_LFRC", "USE_LFSYNT"];

/// nRF51 platform build orchestrator.
pub struct Nrf51Orchestrator;

fn profile_label(profile: fbuild_core::BuildProfile) -> &'static str {
    match profile {
        fbuild_core::BuildProfile::Release => "release",
        fbuild_core::BuildProfile::Quick => "quick",
    }
}

/// ARM GCC toolchain and arduino-nRF5 core for an env, honoring
/// `platform_packages` overrides (FastLED/fbuild#681). Shared by the build
/// and `fbuild install` so both provision the same packages (#1433).
pub(crate) async fn nrf51_packages(
    project_dir: &Path,
    env_config: Option<&HashMap<String, String>>,
) -> Result<(
    fbuild_packages::toolchain::ArmToolchain,
    fbuild_packages::library::Nrf51Cores,
)> {
    let registry_overrides = match env_config {
        Some(env) => {
            crate::package_override::resolve_registry_overrides(
                project_dir,
                env,
                "nordicnrf51",
                &[FRAMEWORK_PACKAGE, "toolchain-gccarmnoneeabi"],
                &[],
            )
            .await?
        }
        None => HashMap::new(),
    };
    Ok(nrf51_packages_from_resolved(
        project_dir,
        env_config,
        &registry_overrides,
    ))
}

fn nrf51_packages_from_resolved(
    project_dir: &Path,
    env_config: Option<&HashMap<String, String>>,
    registry_overrides: &HashMap<String, fbuild_config::PackageOverride>,
) -> (
    fbuild_packages::toolchain::ArmToolchain,
    fbuild_packages::library::Nrf51Cores,
) {
    let toolchain = match registry_overrides.get("toolchain-gccarmnoneeabi").cloned() {
        Some(pin) => fbuild_packages::toolchain::ArmToolchain::with_override(project_dir, pin),
        None => fbuild_packages::toolchain::ArmToolchain::new(project_dir),
    };
    let override_pin = registry_overrides
        .get(FRAMEWORK_PACKAGE)
        .cloned()
        .or_else(|| {
            env_config
                .and_then(|env| crate::package_override::resolve_override(env, FRAMEWORK_PACKAGE))
        });
    let cores = match override_pin {
        Some(o) => fbuild_packages::library::Nrf51Cores::with_override(project_dir, o),
        None => fbuild_packages::library::Nrf51Cores::new(project_dir),
    };
    (toolchain, cores)
}

/// Board defines + series defines, with PlatformIO's `USE_LFXO` default
/// when the board does not pick a low-frequency clock source.
fn nrf51_defines(
    board: &fbuild_config::BoardConfig,
    mcu_config: &crate::generic_arm::ArmMcuConfig,
) -> HashMap<String, String> {
    let mut defines = board.get_defines();
    defines.extend(mcu_config.defines_map());
    if !LF_CLOCK_DEFINES.iter().any(|d| defines.contains_key(*d)) {
        defines.insert("USE_LFXO".to_string(), "1".to_string());
    }
    defines
}

/// Include paths from PlatformIO's nrf5.py `CPPPATH`, plus the variant and
/// the framework's SPI/Wire headers.
fn framework_include_dirs(
    cores: &fbuild_packages::library::Nrf51Cores,
    board: &fbuild_config::BoardConfig,
) -> Vec<NormalizedPath> {
    let sdk = cores.get_sdk_components_dir(&board.core);
    let mut dirs = vec![
        cores.get_core_dir(&board.core),
        cores.get_variant_dir(&board.variant),
        sdk.join("drivers_nrf").join("delay"),
        sdk.join("device"),
        sdk.join("toolchain"),
        sdk.join("toolchain").join("CMSIS").join("Include"),
    ];
    for lib in ["SPI", "Wire"] {
        let dir = cores.get_libraries_dir().join(lib);
        if dir.is_dir() {
            dirs.push(dir);
        }
    }
    dirs
}

fn fingerprint_metadata(
    ctx: &pipeline::BuildContext,
    params: &BuildParams,
    ldscript: &str,
) -> CoreFingerprintMetadata {
    CoreFingerprintMetadata {
        version: BUILD_FINGERPRINT_VERSION,
        env_name: params.env_name.clone(),
        profile: profile_label(params.profile).to_string(),
        board_name: ctx.board.name.clone(),
        board_mcu: ctx.board.mcu.clone(),
        board_define: ctx.board.board.clone(),
        board_core: ctx.board.core.clone(),
        board_f_cpu: ctx.board.f_cpu.clone(),
        board_extra_flags: ctx.board.extra_flags.clone(),
        board_ldscript: Some(ldscript.to_string()),
        board_variant: Some(ctx.board.variant.clone()),
        platform: "nordicnrf51".to_string(),
        max_flash: ctx.board.max_flash,
        max_ram: ctx.board.max_ram,
        eh_frame_policy: None,
        extra: None,
    }
}

fn wants_fast_path(params: &BuildParams) -> bool {
    !params.compiledb_only && !params.symbol_analysis && params.symbol_analysis_path.is_none()
}

#[async_trait::async_trait]
impl BuildOrchestrator for Nrf51Orchestrator {
    fn platform(&self) -> Platform {
        Platform::NordicNrf51
    }

    #[expect(
        clippy::too_many_lines,
        reason = "linear build recipe mirroring the other ARM orchestrators"
    )]
    async fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();
        let mut ctx = pipeline::BuildContext::new(params).await?;

        // Toolchain + arduino-nRF5 core.
        let (toolchain, cores) = nrf51_packages(
            &params.project_dir,
            ctx.config.get_env_config(&params.env_name).ok(),
        )
        .await?;
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain).await?;
        tracing::info!("arm-none-eabi toolchain at {}", toolchain_dir.display());
        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        )
        .await;
        let core_root = fbuild_packages::Package::ensure_installed(&cores).await?;
        tracing::info!("arduino-nRF5 core at {}", core_root.display());

        // Linker script (no SoftDevice): SDK/components/toolchain/gcc/<ldscript>.
        let ldscript = ctx
            .board
            .ldscript
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_LDSCRIPT.to_string());
        let linker_script_path = cores.get_linker_script(&ctx.board.core, &ldscript);
        if !linker_script_path.is_file() {
            return Err(FbuildError::ConfigError(format!(
                "arduino-nRF5 linker script not found: {} (board ldscript '{}')",
                linker_script_path.display(),
                ldscript
            )));
        }

        let metadata_hash =
            stable_hash_with_build_config(&fingerprint_metadata(&ctx, params, &ldscript), &ctx)?;
        let (fast_elf, [fast_hex], fast_compile_db) =
            expected_fast_path_artifacts(&ctx.build_dir, &params.project_dir, ["firmware.hex"]);
        let fast_path = FastPathContract::for_project_outputs(
            &ctx.build_dir,
            &params.project_dir,
            [fast_elf.clone(), fast_hex.clone(), fast_compile_db.clone()],
        );
        if wants_fast_path(params) {
            let inputs = FastPathCheckInputs {
                metadata_hash: &metadata_hash,
                extra_artifact_ok: None,
                watch_set_cache: params.watch_set_cache.as_deref(),
                compiler_cache: None,
            };
            if let Some(hit) = crate::build_fingerprint::fast_path_check(&fast_path, &inputs)? {
                return Ok(crate::build_fingerprint::assemble_fast_path_result(
                    hit,
                    ctx.build_log,
                    crate::build_fingerprint::FastPathResultInputs {
                        platform_label: "NRF51",
                        mcu: &ctx.board.mcu,
                        env_name: &params.env_name,
                        firmware_path: fast_hex,
                        elf_path: fast_elf,
                        compile_database_path: fast_compile_db,
                        elapsed: start.elapsed().as_secs_f64(),
                    },
                ));
            }
        }

        // Sources: sketch + recursive core (incl. SDK startup/system) + variant.
        let core_dir = cores.get_core_dir(&ctx.board.core);
        let variant_dir = cores.get_variant_dir(&ctx.board.variant);
        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let sources = scanner.scan_all_filtered(
            Some(&core_dir),
            Some(&variant_dir),
            ctx.source_filter.as_deref(),
        )?;
        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        let mcu_config = get_nrf51_config_for_mcu(&ctx.board.mcu)?;
        let defines = nrf51_defines(&ctx.board, &mcu_config);
        let mut include_dirs: Vec<_> = framework_include_dirs(&cores, &ctx.board)
            .into_iter()
            .map(NormalizedPath::into_path_buf)
            .collect();
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs)?;
        include_dirs.extend(toolchain.get_include_dirs());

        let new_compiler = |include_dirs| {
            ArmCompiler::new(
                toolchain.get_gcc_path(),
                toolchain.get_gxx_path(),
                &ctx.board.mcu,
                &ctx.board.f_cpu,
                defines.clone(),
                include_dirs,
                mcu_config.clone(),
                params.profile,
                params.verbose,
            )
        };

        // Resolve lib_deps before the final compiler so their headers are visible (#1276).
        let lib_deps = ctx.config.get_lib_deps(&params.env_name)?;
        let lib_ignore = ctx
            .config
            .get_lib_ignore(&params.env_name)
            .unwrap_or_default();
        let lib_deps_plan = if lib_deps.is_empty() {
            pipeline::LibDeps::default()
        } else {
            let temp_compiler = new_compiler(include_dirs.clone());
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
                None,
            )
            .await?
        };

        let compiler =
            new_compiler(include_dirs.clone()).with_build_unflags(ctx.build_unflags.clone());
        let linker = ArmLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script_path.into_path_buf(),
            mcu_config,
            params.profile,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        )
        .with_lib_search_dirs(vec![cores.get_linker_dir(&ctx.board.core).into_path_buf()]);

        let gcc_path = toolchain.get_gcc_path();
        let gxx_path = toolchain.get_gxx_path();
        let ar_path = toolchain.get_ar_path();
        let gcc_ar_path = toolchain.get_gcc_ar_path();
        let c_flags = crate::compiler::Compiler::c_flags(&compiler);
        let cpp_flags = crate::compiler::Compiler::cpp_flags(&compiler);
        let lib_env = pipeline::LibraryBuildEnv {
            gcc_path: &gcc_path,
            gxx_path: &gxx_path,
            ar_path: pipeline::pick_archiver(&ar_path, &gcc_ar_path, &c_flags, &cpp_flags),
            c_flags: &c_flags,
            cpp_flags: &cpp_flags,
            include_dirs: &include_dirs,
            verbose: params.verbose,
            jobs: crate::parallel::effective_jobs(params.jobs),
            compiler_cache: None,
        };

        let build_result = pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &[],
            lib_deps_plan,
            Some(&lib_env),
            TargetArchitecture::Arm,
            "NRF51",
            start,
        )
        .await?;

        if build_result.success && wants_fast_path(params) {
            crate::build_fingerprint::persist_fast_path_success(
                &fast_path,
                &FastPathPersistInputs {
                    metadata_hash: &metadata_hash,
                    size_info: build_result.size_info.clone(),
                    watch_set_cache: params.watch_set_cache.as_deref(),
                    compiler_cache: None,
                },
            );
        }

        Ok(build_result)
    }
}

/// Create an nRF51 orchestrator (dispatch-table entry point).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Nrf51Orchestrator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestrator_reports_nrf51_platform() {
        assert_eq!(Nrf51Orchestrator.platform(), Platform::NordicNrf51);
    }

    #[test]
    fn nrf51_consumes_registry_framework_and_toolchain() {
        let temp = tempfile::TempDir::new().unwrap();
        let resolved = HashMap::from([
            (
                FRAMEWORK_PACKAGE.to_string(),
                fbuild_config::PackageOverride::new("https://example.test/nrf5.tar.gz", "1.800.0"),
            ),
            (
                "toolchain-gccarmnoneeabi".to_string(),
                fbuild_config::PackageOverride::new("https://example.test/gcc.tar.gz", "1.70201.0"),
            ),
        ]);
        let (toolchain, cores) = nrf51_packages_from_resolved(temp.path(), None, &resolved);
        assert_eq!(
            fbuild_packages::Package::get_info(&toolchain).version,
            "1.70201.0"
        );
        assert_eq!(
            fbuild_packages::Package::get_info(&cores).version,
            "1.800.0"
        );
    }

    #[test]
    fn bbcmicrobit_keeps_board_lfrc_and_gets_series_defines() {
        let board = fbuild_test_support::board_for_test("bbcmicrobit");
        let config = get_nrf51_config_for_mcu(&board.mcu).unwrap();
        let defines = nrf51_defines(&board, &config);
        assert!(defines.contains_key("USE_LFRC"));
        assert!(!defines.contains_key("USE_LFXO"));
        assert!(defines.contains_key("NRF51"));
        assert!(defines.contains_key("ARDUINO_BBC_MICROBIT"));
        assert_eq!(defines.get("ARDUINO"), Some(&"10805".to_string()));
    }

    #[test]
    fn defaults_to_lfxo_when_board_selects_no_clock() {
        let mut board = fbuild_test_support::board_for_test("bbcmicrobit");
        board.extra_flags = None;
        let config = get_nrf51_config_for_mcu(&board.mcu).unwrap();
        assert!(nrf51_defines(&board, &config).contains_key("USE_LFXO"));
    }
}
