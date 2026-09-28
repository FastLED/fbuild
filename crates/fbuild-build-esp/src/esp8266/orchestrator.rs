//! ESP8266 build orchestrator â€” wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config
//! 3. Ensure xtensa-lx106-elf toolchain
//! 4. Ensure Arduino ESP8266 framework
//! 5. Load MCU config from embedded JSON
//! 6. Scan source files
//! 7. Build include dirs + compiler + linker
//! 8. Run shared sequential build pipeline

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};
use fbuild_packages::Framework as _;

use crate::build_fingerprint::{
    CoreFingerprintMetadata, FastPathCheckInputs, FastPathContract, FastPathPersistInputs,
    expected_fast_path_artifacts, stable_hash_with_build_config,
};
use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::esp8266_compiler::Esp8266Compiler;
use super::esp8266_linker::Esp8266Linker;
use super::mcu_config::get_esp8266_config;

/// ESP8266 platform build orchestrator.
pub struct Esp8266Orchestrator;

fn profile_label(profile: fbuild_core::BuildProfile) -> &'static str {
    match profile {
        fbuild_core::BuildProfile::Release => "release",
        fbuild_core::BuildProfile::Quick => "quick",
    }
}

/// Resolve the selected PlatformIO platform's exact framework and toolchain
/// payloads before constructing either package. Shared by build and install.
pub(crate) async fn esp8266_packages(
    project_dir: &Path,
    env_config: Option<&HashMap<String, String>>,
) -> Result<(
    fbuild_packages::toolchain::Esp8266Toolchain,
    fbuild_packages::library::Esp8266Framework,
)> {
    esp8266_packages_with_fetch(project_dir, env_config, true)
        .await?
        .ok_or_else(|| {
            fbuild_core::FbuildError::PackageError("ESP8266 packages unavailable".into())
        })
}

pub(crate) async fn esp8266_packages_offline(
    project_dir: &Path,
    env_config: Option<&HashMap<String, String>>,
) -> Result<
    Option<(
        fbuild_packages::toolchain::Esp8266Toolchain,
        fbuild_packages::library::Esp8266Framework,
    )>,
> {
    esp8266_packages_with_fetch(project_dir, env_config, false).await
}

async fn esp8266_packages_with_fetch(
    project_dir: &Path,
    env_config: Option<&HashMap<String, String>>,
    fetch: bool,
) -> Result<
    Option<(
        fbuild_packages::toolchain::Esp8266Toolchain,
        fbuild_packages::library::Esp8266Framework,
    )>,
> {
    let registry_overrides = if let Some(env) = env_config {
        if fetch {
            Some(
                crate::package_override::resolve_registry_overrides(
                    project_dir,
                    env,
                    "espressif8266",
                    &["framework-arduinoespressif8266", "toolchain-xtensa"],
                    &[],
                )
                .await?,
            )
        } else {
            crate::package_override::resolve_registry_overrides_offline(
                project_dir,
                env,
                "espressif8266",
                &["framework-arduinoespressif8266", "toolchain-xtensa"],
                &[],
            )
            .await?
        }
    } else {
        Some(HashMap::new())
    };
    Ok(registry_overrides.map(|registry_overrides| {
        esp8266_packages_from_resolved(project_dir, env_config, &registry_overrides)
    }))
}

fn esp8266_packages_from_resolved(
    project_dir: &Path,
    env_config: Option<&HashMap<String, String>>,
    registry_overrides: &HashMap<String, fbuild_config::PackageOverride>,
) -> (
    fbuild_packages::toolchain::Esp8266Toolchain,
    fbuild_packages::library::Esp8266Framework,
) {
    let toolchain_pin = registry_overrides
        .get("toolchain-xtensa")
        .cloned()
        .or_else(|| {
            env_config
                .and_then(|env| crate::package_override::resolve_override(env, "toolchain-xtensa"))
        });
    let toolchain = match toolchain_pin {
        Some(pin) => fbuild_packages::toolchain::Esp8266Toolchain::with_override(project_dir, pin),
        None => fbuild_packages::toolchain::Esp8266Toolchain::new(project_dir),
    };
    let override_pin = registry_overrides
        .get("framework-arduinoespressif8266")
        .cloned()
        .or_else(|| {
            env_config.and_then(|env| {
                crate::package_override::resolve_override(env, "framework-arduinoespressif8266")
            })
        });
    let framework = match override_pin {
        Some(o) => fbuild_packages::library::Esp8266Framework::with_override(project_dir, o),
        None => fbuild_packages::library::Esp8266Framework::new(project_dir),
    };
    (toolchain, framework)
}

/// Read the registry identity already resolved by `esp8266_packages` for
/// build output and the fast-path fingerprint. This is cache-only.
async fn selected_platform_identity(
    project_dir: &Path,
    requested: &str,
) -> Result<Option<fbuild_core::platformio_package::ResolvedPayload>> {
    use fbuild_core::platformio_package::{PackageKind, host_system, parse_package_spec};

    let spec = parse_package_spec(requested)
        .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?;
    let Some(registry) = spec
        .registry()
        .filter(|registry| registry.requirement.is_some())
    else {
        return Ok(None);
    };
    let host = host_system(fbuild_core::platform::host::current()).ok_or_else(|| {
        fbuild_core::FbuildError::PackageError("unsupported PlatformIO host".into())
    })?;
    let cache_root = fbuild_packages::Cache::new(project_dir).platforms_dir();
    fbuild_packages::platformio_registry::RegistryClient::default()
        .resolve_cached(registry, PackageKind::Platform, host, &cache_root, false)
        .await
        .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?
        .ok_or_else(|| {
            fbuild_core::FbuildError::PackageError(format!(
                "selected ESP8266 platform `{requested}` missing from registry cache"
            ))
        })
        .map(Some)
}

#[async_trait::async_trait]
impl BuildOrchestrator for Esp8266Orchestrator {
    fn platform(&self) -> Platform {
        Platform::Espressif8266
    }

    async fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params).await?;

        // Compute eh_frame strip policy once per build (FastLED/fbuild#244).
        // No sdkconfig on ESP8266.
        let eh_frame_policy =
            crate::eh_frame_policy_compute::compute_eh_frame_policy(&ctx, params.profile, None);

        // 3-4. Toolchain and framework
        let (toolchain, framework) = esp8266_packages(
            &params.project_dir,
            ctx.config.get_env_config(&params.env_name).ok(),
        )
        .await?;
        let toolchain_info = fbuild_packages::Package::get_info(&toolchain);
        let framework_info = fbuild_packages::Package::get_info(&framework);
        let requested_platform = ctx
            .config
            .get_env_config(&params.env_name)?
            .get("platform")
            .cloned()
            .unwrap_or_else(|| "espressif8266".to_string());
        let selected_platform =
            selected_platform_identity(&params.project_dir, &requested_platform).await?;
        ctx.build_log
            .push(format!("ESP8266 requested platform: {requested_platform}"));
        if let Some(selected) = &selected_platform {
            ctx.build_log.push(format!(
                "ESP8266 resolved platform: {}/{}@{}: {} (sha256 {})",
                selected.owner, selected.name, selected.version, selected.url, selected.sha256,
            ));
        }
        ctx.build_log.push(format!(
            "ESP8266 toolchain-xtensa@{}: {} (sha256 {})",
            toolchain_info.version,
            toolchain_info.url,
            toolchain_info.checksum.as_deref().unwrap_or("unverified"),
        ));
        ctx.build_log.push(format!(
            "ESP8266 framework-arduinoespressif8266@{}: {} (sha256 {})",
            framework_info.version,
            framework_info.url,
            framework_info.checksum.as_deref().unwrap_or("unverified"),
        ));
        let _toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain).await?;
        tracing::info!("ESP8266 toolchain ready");

        use fbuild_packages::Toolchain as _;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "xtensa-lx106-elf-gcc",
            &mut ctx.build_log,
        )
        .await;

        let _framework_dir = fbuild_packages::Package::ensure_installed(&framework).await?;
        tracing::info!("ESP8266 framework ready");
        let board_id = ctx
            .config
            .get_env_config(&params.env_name)?
            .get("board")
            .cloned()
            .unwrap_or_default();
        let board_props = crate::arduino_props::load_board_props_with_default_menus(
            &framework.get_boards_txt(),
            &board_id,
        );

        let core_dir = framework.get_core_dir(&ctx.board.core);
        let variant_dir = framework.get_variant_dir(&ctx.board.variant);

        // 5. Load MCU config
        let mut mcu_config = get_esp8266_config()?;
        apply_esp8266_board_props(&board_props, &mut mcu_config);

        // Compute flash_freq early for the fast-path fingerprint (also used by
        // the linker constructor below).
        let f_for_image = ctx
            .board
            .f_image
            .as_deref()
            .or(ctx.board.f_flash.as_deref());
        let flash_freq = crate::esp32::esp32_linker::f_flash_to_esptool_freq(
            f_for_image,
            &mcu_config.esptool.default_flash_freq,
        );

        let build_dir = &ctx.build_dir;
        let metadata_hash = stable_hash_with_build_config(
            &CoreFingerprintMetadata {
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
                platform: "esp8266".to_string(),
                max_flash: ctx.board.max_flash,
                max_ram: ctx.board.max_ram,
                eh_frame_policy: Some(match eh_frame_policy {
                    crate::eh_frame_policy::EhFramePolicy::Strip => "strip".to_string(),
                    crate::eh_frame_policy::EhFramePolicy::Preserve => "preserve".to_string(),
                }),
                extra: Some(std::collections::BTreeMap::from([
                    ("requested_platform".to_string(), requested_platform.clone()),
                    (
                        "resolved_platform".to_string(),
                        selected_platform
                            .as_ref()
                            .map(|payload| {
                                format!(
                                    "{}/{}@{}#{}",
                                    payload.owner, payload.name, payload.version, payload.sha256
                                )
                            })
                            .unwrap_or_default(),
                    ),
                    (
                        "flash_mode".to_string(),
                        ctx.board.flash_mode.clone().unwrap_or_default(),
                    ),
                    ("flash_freq".to_string(), flash_freq.clone()),
                    (
                        "toolchain_package".to_string(),
                        format!(
                            "{}@{}#{}",
                            toolchain_info.url,
                            toolchain_info.version,
                            toolchain_info.checksum.as_deref().unwrap_or("unverified")
                        ),
                    ),
                    (
                        "framework_package".to_string(),
                        format!(
                            "{}@{}#{}",
                            framework_info.url,
                            framework_info.version,
                            framework_info.checksum.as_deref().unwrap_or("unverified")
                        ),
                    ),
                ])),
            },
            &ctx,
        )?;
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
                        platform_label: "ESP8266",
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

        // 6. Scan sources
        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let variant_dir_opt = if variant_dir.exists() {
            Some(variant_dir.as_path())
        } else {
            None
        };
        let sources = scanner.scan_all_filtered(
            Some(&core_dir),
            variant_dir_opt,
            ctx.source_filter.as_deref(),
        )?;

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 7. Build include dirs + defines
        let mut defines = ctx.board.get_defines();
        apply_define_flags_from_props(&board_props, &mut defines);
        apply_esp8266_board_identity(&board_props, &board_id, &mut defines);
        defines.extend(mcu_config.defines_map());
        let mut include_dirs = vec![core_dir.clone()];
        if variant_dir.exists() {
            include_dirs.push(variant_dir.clone());
        }
        // SDK include paths
        include_dirs.extend(framework.get_sdk_include_dirs());
        // Toolchain sysroot includes (xtensa/coreasm.h, etc.)
        // Required by .S assembly files â€” see platform.txt compiler.S.flags.
        include_dirs.extend(toolchain.get_include_dirs());
        // SDK libc headers (platform.txt compiler.libc.path)
        include_dirs.extend(framework.get_libc_include_dirs());
        // Built-in Arduino libraries (ESP8266WiFi, etc.)
        let builtin_libs_dir = framework.get_libraries_dir();
        if builtin_libs_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&builtin_libs_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let lib_src = path.join("src");
                        if lib_src.is_dir() {
                            include_dirs.push(lib_src);
                        }
                    }
                }
            }
        }
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);

        // 6a. Download `lib_deps` from the registry / remote URLs before
        // creating the compiler, so the downloaded library include directories
        // are available during compilation (FastLED/fbuild#1276).
        let lib_deps = ctx.config.get_lib_deps(&params.env_name)?;
        let lib_ignore = ctx
            .config
            .get_lib_ignore(&params.env_name)
            .unwrap_or_default();
        let lib_deps_plan = if !lib_deps.is_empty() {
            let temp_compiler = Esp8266Compiler::new(
                toolchain.get_gcc_path(),
                toolchain.get_gxx_path(),
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
                None,
            )
            .await?
        } else {
            pipeline::LibDeps::default()
        };

        let compiler = Esp8266Compiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
        )
        .with_build_unflags(ctx.build_unflags.clone())
        .with_eh_frame_policy(eh_frame_policy);

        // Resolve linker script from board config
        let ldscript = ctx
            .board
            .ldscript
            .as_deref()
            .unwrap_or("eagle.flash.4m1m.ld");
        let sdk_ld_dir = framework.get_sdk_ld_dir();
        let linker_scripts = crate::linker::LinkerScripts::single(sdk_ld_dir.clone(), ldscript);

        // flash_freq was computed above for the fast-path fingerprint; reuse here.
        let sdk_name = esp8266_sdk_name(&mcu_config).to_string();
        let linker = Esp8266Linker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            framework.get_sdk_lib_dir(),
            framework.get_sdk_nonosdk_lib_dir_for(&sdk_name),
            sdk_ld_dir,
            linker_scripts,
            mcu_config,
            params.profile,
            ctx.board.flash_mode.clone(),
            &flash_freq,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        )
        .with_caller_path(params.caller_path.clone());

        // 8. Build LibraryBuildEnv for project-as-library compilation
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

        // 9. Run shared sequential build pipeline
        let result = pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &[],
            lib_deps_plan,
            Some(&lib_env),
            TargetArchitecture::Xtensa,
            "ESP8266",
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

fn apply_define_flags_from_props(
    board_props: &Option<HashMap<String, String>>,
    defines: &mut HashMap<String, String>,
) {
    let Some(props) = board_props.as_ref() else {
        return;
    };
    for key in [
        "flash_flags",
        "lwip_flags",
        "mmuflags",
        "debug_port",
        "debug_level",
        "vtable_flags",
    ] {
        if let Some(flags) = props.get(key) {
            let tokens = fbuild_core::shell_split::split(flags);
            for token in tokens {
                if let Some(def) = token.strip_prefix("-D") {
                    if let Some((name, value)) = def.split_once('=') {
                        defines.insert(name.to_string(), value.to_string());
                    } else {
                        defines.insert(def.to_string(), "1".to_string());
                    }
                }
            }
        }
    }
}

fn apply_esp8266_board_props(
    board_props: &Option<HashMap<String, String>>,
    mcu_config: &mut super::mcu_config::Esp8266McuConfig,
) {
    let Some(props) = board_props.as_ref() else {
        return;
    };

    if let Some(sdk_name) = props.get("sdk") {
        mcu_config
            .defines
            .retain(|entry| !matches!(entry, crate::esp32::mcu_config::DefineEntry::KeyValue(name, _) if name.starts_with("NONOSDK")));
        mcu_config
            .defines
            .push(crate::esp32::mcu_config::DefineEntry::KeyValue(
                sdk_name.clone(),
                "1".to_string(),
            ));
    }

    for key in ["flash_flags", "lwip_flags", "mmuflags", "vtable_flags"] {
        if let Some(flags) = props.get(key) {
            for token in fbuild_core::shell_split::split(flags) {
                if let Some(def) = token.strip_prefix("-D") {
                    let (name, value) = def
                        .split_once('=')
                        .map(|(name, value)| (name.to_string(), value.to_string()))
                        .unwrap_or_else(|| (def.to_string(), "1".to_string()));
                    mcu_config.defines.retain(|entry| match entry {
                        crate::esp32::mcu_config::DefineEntry::Simple(existing) => {
                            existing != &name
                        }
                        crate::esp32::mcu_config::DefineEntry::KeyValue(existing, _) => {
                            existing != &name
                        }
                    });
                    mcu_config
                        .defines
                        .push(crate::esp32::mcu_config::DefineEntry::KeyValue(name, value));
                }
            }
        }
    }

    if let Some(lwip_lib) = props.get("lwip_lib") {
        for lib in &mut mcu_config.linker_libs {
            if lib.starts_with("-llwip") {
                *lib = lwip_lib.clone();
                break;
            }
        }
    }
    if let Some(stdcpp_lib) = props.get("stdcpp_lib") {
        for lib in &mut mcu_config.linker_libs {
            if lib == "-lstdc++" || lib == "-lstdc++-exc" {
                *lib = stdcpp_lib.clone();
                break;
            }
        }
    }
}

fn apply_esp8266_board_identity(
    board_props: &Option<HashMap<String, String>>,
    board_id: &str,
    defines: &mut HashMap<String, String>,
) {
    if let Some(props) = board_props.as_ref() {
        if let Some(board_define) = props.get("board") {
            defines.insert(
                format!("ARDUINO_{}", board_define.to_uppercase()),
                "1".to_string(),
            );
        }
    }

    defines.insert(
        "ARDUINO_BOARD".to_string(),
        format!("\\\"PLATFORMIO_{}\\\"", board_id.to_uppercase()),
    );
    defines.insert(
        "ARDUINO_BOARD_ID".to_string(),
        format!("\\\"{}\\\"", board_id),
    );
}

fn esp8266_sdk_name(mcu_config: &super::mcu_config::Esp8266McuConfig) -> &str {
    mcu_config
        .defines
        .iter()
        .find_map(|entry| match entry {
            crate::esp32::mcu_config::DefineEntry::KeyValue(name, _)
                if name.starts_with("NONOSDK") =>
            {
                Some(name.as_str())
            }
            _ => None,
        })
        .unwrap_or("NONOSDK22x_190703")
}

/// Create an ESP8266 orchestrator.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Esp8266Orchestrator)
}

/// Check if a project is configured for ESP8266 by reading its platformio.ini.
pub fn is_esp8266_project(project_dir: &Path, env_name: &str) -> bool {
    pipeline::is_platform_project(project_dir, env_name, Platform::Espressif8266)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_packages::Package as _;

    #[test]
    fn explicit_toolchain_registry_pin_does_not_use_fixed_github_toolchain() {
        let temp = tempfile::tempdir().unwrap();
        let env = HashMap::from([
            ("platform".to_string(), "espressif8266".to_string()),
            (
                "platform_packages".to_string(),
                "platformio/toolchain-xtensa@2.100300.220621".to_string(),
            ),
        ]);
        let registry_overrides = HashMap::from([(
            "toolchain-xtensa".to_string(),
            fbuild_config::PackageOverride {
                url: "https://dl.registry.platformio.org/download/platformio/tool/toolchain-xtensa/2.100300.220621/toolchain-xtensa-linux_x86_64-2.100300.220621.tar.gz".to_string(),
                version: "2.100300.220621".to_string(),
                checksum: Some("a3d51bebcfaa2f5cca154956fee3e9270b6d0e9c5d51de6034a86aaa606ea8a5".to_string()),
            },
        )]);
        let (toolchain, _) =
            esp8266_packages_from_resolved(temp.path(), Some(&env), &registry_overrides);
        assert_eq!(toolchain.get_info().version, "2.100300.220621");
        assert!(
            toolchain
                .get_info()
                .url
                .contains("dl.registry.platformio.org")
        );
        assert_eq!(
            toolchain.get_info().checksum,
            registry_overrides["toolchain-xtensa"].checksum
        );
    }

    #[test]
    fn framework_url_override_precedes_selected_platform_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let url = "https://example.test/esp8266-framework.tar.gz";
        let env = HashMap::from([
            ("platform".to_string(), "espressif8266@4.0.1".to_string()),
            (
                "platform_packages".to_string(),
                format!("framework-arduinoespressif8266@{url}"),
            ),
        ]);
        let registry_overrides = HashMap::from([(
            "toolchain-xtensa".to_string(),
            fbuild_config::PackageOverride::new(
                "https://example.test/toolchain.tar.gz",
                "2.100300.220621",
            ),
        )]);
        let (toolchain, framework) =
            esp8266_packages_from_resolved(temp.path(), Some(&env), &registry_overrides);
        assert_eq!(framework.get_info().url, url);
        assert_eq!(toolchain.get_info().version, "2.100300.220621");
    }

    #[tokio::test]
    async fn unpinned_platform_preserves_framework_archive_override() {
        let temp = tempfile::tempdir().unwrap();
        let url = "https://example.test/esp8266-framework.tar.gz";
        let env = HashMap::from([
            ("platform".to_string(), "espressif8266".to_string()),
            (
                "platform_packages".to_string(),
                format!("framework-arduinoespressif8266@{url}"),
            ),
        ]);
        let (_, framework) = esp8266_packages(temp.path(), Some(&env)).await.unwrap();
        assert_eq!(framework.get_info().url, url);
    }

    #[tokio::test]
    async fn unrelated_registry_platform_fails_before_package_install() {
        let temp = tempfile::tempdir().unwrap();
        let env = HashMap::from([("platform".to_string(), "unknown8266@4.0.1".to_string())]);
        let error = esp8266_packages(temp.path(), Some(&env))
            .await
            .err()
            .expect("wrong platform must fail");
        assert!(
            error
                .to_string()
                .contains("expected PlatformIO platform `espressif8266`")
        );
    }

    #[tokio::test]
    async fn unsupported_registry_package_fails_before_download() {
        let temp = tempfile::tempdir().unwrap();
        let env = HashMap::from([
            ("platform".to_string(), "espressif8266".to_string()),
            (
                "platform_packages".to_string(),
                "platformio/tool-esptoolpy@1.30000.201119".to_string(),
            ),
        ]);
        let error = esp8266_packages(temp.path(), Some(&env))
            .await
            .err()
            .expect("unsupported package must fail");
        assert!(error.to_string().contains("tool-esptoolpy"));
        assert!(error.to_string().contains("not supported"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads PlatformIO ESP8266 4.0.1 platform, GCC, and Arduino framework"]
    async fn pinned_esp8266_401_builds_with_selected_registry_packages() {
        let backend = crate::compile_backend::CompileBackend::start()
            .await
            .expect("compile backend starts");
        crate::compile_backend::install_global(backend);
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(
            project.path().join("platformio.ini"),
            "[env:esp8266]\nplatform = espressif8266@4.0.1\nboard = nodemcuv2\nframework = arduino\n",
        )
        .unwrap();
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/platform/esp8266/src/main.ino"
            ),
            project.path().join("src/main.ino"),
        )
        .unwrap();
        let build_dir = fbuild_paths::BuildLayout::new(
            project.path().to_path_buf(),
            "esp8266".into(),
            fbuild_core::BuildProfile::Release,
        )
        .resolve();
        let params = BuildParams {
            project_dir: project.path().to_path_buf(),
            env_name: "esp8266".into(),
            clean_all: false,
            clean_only: false,
            clean: false,
            profile: fbuild_core::BuildProfile::Release,
            build_dir,
            verbose: false,
            jobs: Some(2),
            generate_compiledb: false,
            compiledb_only: false,
            log_sender: None,
            symbol_analysis: false,
            symbol_analysis_path: None,
            no_timestamp: true,
            src_dir: None,
            pio_env: Default::default(),
            extra_build_flags: Vec::new(),
            watch_set_cache: None,
            bloat_analysis: false,
            caller_path: None,
        };
        let built = Esp8266Orchestrator.build(&params).await.unwrap();
        assert!(built.success);
        assert!(built.elf_path.as_ref().is_some_and(|path| path.is_file()));
        let config =
            fbuild_config::PlatformIOConfig::from_path(&project.path().join("platformio.ini"))
                .unwrap();
        let env = config.get_env_config("esp8266").unwrap();
        let (cached_toolchain, cached_framework) =
            esp8266_packages_offline(project.path(), Some(env))
                .await
                .unwrap()
                .expect("installed platform and payload metadata resolve offline");
        assert_eq!(
            fbuild_packages::Package::get_info(&cached_toolchain).version,
            "2.100300.220621"
        );
        assert_eq!(
            fbuild_packages::Package::get_info(&cached_framework).version,
            "3.30002.0"
        );
        let (_, framework) = esp8266_packages(project.path(), Some(env)).await.unwrap();
        let core_version =
            std::fs::read_to_string(framework.get_core_dir("esp8266").join("core_version.h"))
                .unwrap();
        assert!(core_version.contains("ARDUINO_ESP8266_RELEASE   \"3.0.2\""));
        let log = built.build_log.into_lines().join("\n");
        assert!(
            log.contains("ESP8266 requested platform: espressif8266@4.0.1"),
            "{log}"
        );
        assert!(
            log.contains("ESP8266 resolved platform: platformio/espressif8266@4.0.1"),
            "{log}"
        );
        assert!(
            log.contains("framework-arduinoespressif8266@3.30002.0"),
            "{log}"
        );
        assert!(log.contains("toolchain-xtensa@2.100300.220621"), "{log}");
        assert!(!log.contains("version pin `4.0.1` is ignored"), "{log}");
    }

    #[test]
    fn test_esp8266_orchestrator_platform() {
        let orch = Esp8266Orchestrator;
        assert_eq!(orch.platform(), Platform::Espressif8266);
    }

    #[test]
    fn test_is_esp8266_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:esp8266]\nplatform = espressif8266\nboard = nodemcuv2\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_esp8266_project(tmp.path(), "esp8266"));
        assert!(!is_esp8266_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_is_not_esp8266_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:esp32]\nplatform = espressif32\nboard = esp32dev\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_esp8266_project(tmp.path(), "esp32"));
    }
}
