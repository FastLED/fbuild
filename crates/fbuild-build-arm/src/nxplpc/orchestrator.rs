//! NXP LPC8xx build orchestrator â€” Stage 2 of #487.
//!
//! Compiles user sketch sources (.ino â†’ .cpp + .c + .cpp + .S) together
//! with the per-MCU startup `.S` and the hand-rolled Arduino `main.cpp`
//! shim, links against the per-MCU linker script, and emits `firmware.elf`
//! + `firmware.bin` via objcopy.
//!
//! No external framework is required at this stage â€” the test fixtures
//! (`tests/platform/lpc845/lpc845.ino`,
//! `tests/platform/lpc804/lpc804.ino`) are 3-line `setup()`/`loop()` stubs.
//! Stage 3 (#479) replaces the embedded shim with the framework-owned
//! `main()` from [`zackees/ArduinoCore-LPC8xx`](https://github.com/zackees/ArduinoCore-LPC8xx).
//!
//! Pattern mirrors the Apollo3 orchestrator
//! (`crates/fbuild-build/src/apollo3/orchestrator.rs`) â€” same Cortex-M
//! family, same `generic_arm::ArmCompiler` + `ArmLinker` pipeline â€” minus
//! the mbed-os framework machinery that Apollo3 needs.

use std::path::PathBuf;
use std::time::Instant;

use fbuild_core::{FbuildError, Platform, Result};

use crate::build_fingerprint::{
    CoreFingerprintMetadata, FastPathCheckInputs, FastPathContract, FastPathPersistInputs,
    expected_fast_path_artifacts, stable_hash_json,
};
use crate::compile_database::TargetArchitecture;
use crate::flag_overlay::apply_overlay_flags;
use crate::generic_arm::{ArmCompiler, ArmLinker};
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::mcu_config;

fn board_lpc_family(board: &fbuild_config::BoardConfig) -> Result<&'static str> {
    let mut candidates = vec![
        board.mcu.as_str(),
        board.variant.as_str(),
        board.board.as_str(),
    ];
    if let Some(ldscript) = board.ldscript.as_deref() {
        candidates.push(ldscript);
    }
    for candidate in candidates {
        let lower = candidate.to_ascii_lowercase();
        if lower.contains("lpc804") {
            return Ok("lpc804");
        }
        if lower.contains("lpc845") {
            return Ok("lpc845");
        }
    }
    Err(FbuildError::ConfigError(format!(
        "unknown NXP LPC8xx board '{}' (mcu '{}', variant '{}'); expected LPC804 or LPC845 metadata",
        board.name, board.mcu, board.variant
    )))
}

/// Enumerate compilable translation units (`.c/.cc/.cpp/.S/.s`) directly
/// inside `dir`. Pulls the vendored ArduinoCore-LPC8xx core sources into the
/// build as "core" sources.
fn collect_compilable_sources(dir: &std::path::Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| {
        FbuildError::BuildFailed(format!("failed to read core dir {}: {}", dir.display(), e))
    })? {
        let path = entry
            .map_err(|e| FbuildError::BuildFailed(format!("core dir entry error: {}", e)))?
            .path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("c" | "cc" | "cpp" | "S" | "s")
            )
        {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// NXP LPC8xx (Cortex-M0+) build orchestrator.
pub struct NxpLpcOrchestrator;

fn profile_label(profile: fbuild_core::BuildProfile) -> &'static str {
    match profile {
        fbuild_core::BuildProfile::Release => "release",
        fbuild_core::BuildProfile::Quick => "quick",
    }
}

#[async_trait::async_trait]
impl BuildOrchestrator for NxpLpcOrchestrator {
    fn platform(&self) -> Platform {
        Platform::NxpLpc
    }

    async fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse platformio.ini, load board, setup build dirs.
        let mut ctx = pipeline::BuildContext::new(params).await?;

        // eh_frame strip policy â€” same convention every other orchestrator
        // follows (#244).
        let eh_frame_policy =
            crate::eh_frame_policy_compute::compute_eh_frame_policy(&ctx, params.profile, None);

        // 3. Ensure ARM GCC. `install_deps` already pre-installs this when
        // the platform is dispatched, but ensure_installed is idempotent
        // and cheap when the toolchain is already on disk.
        let toolchain = fbuild_packages::toolchain::ArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain).await?;
        tracing::info!("arm-none-eabi-gcc toolchain at {}", toolchain_dir.display());

        let cmsis = fbuild_packages::library::CmsisFramework::new(&params.project_dir);
        let cmsis_dir = fbuild_packages::Package::ensure_installed(&cmsis).await?;
        tracing::info!("CMSIS framework at {}", cmsis_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        )
        .await;

        // 4. Vendor the Arduino LPC8xx core framework. This supersedes the
        //    embedded `arduino_stub/` shim (FastLED/fbuild#479, #487): the
        //    framework owns `main()`, startup + vector table, wiring,
        //    HardwareSerial, SPI, and the device headers.
        //
        //    Honor `platform_packages = framework-arduino-lpc8xx@<URL>#<sha>`
        //    from the env section (FastLED/fbuild#663, #681): if set, the
        //    override URL replaces the const-pinned default and gets its own
        //    cache subdir via `PackageBase::with_override`. The parser
        //    + resolver are shared across every framework orchestrator so
        //    nxplpc carries no platform-specific platform_packages logic.
        let core_override = ctx
            .config
            .get_env_config(&params.env_name)
            .ok()
            .and_then(|env| {
                crate::package_override::resolve_override(env, "framework-arduino-lpc8xx")
            });
        let core = match core_override {
            Some(ovr) => {
                let banner = format!(
                    "ArduinoCore-LPC8xx OVERRIDE: {} (default pinned: {})",
                    ovr.url,
                    fbuild_packages::library::ArduinoCoreLpc8xx::commit()
                );
                ctx.build_log.push(banner);
                fbuild_packages::library::ArduinoCoreLpc8xx::with_override(&params.project_dir, ovr)
            }
            None => fbuild_packages::library::ArduinoCoreLpc8xx::new(&params.project_dir),
        };
        let core_root = fbuild_packages::Package::ensure_installed(&core).await?;
        tracing::info!("ArduinoCore-LPC8xx at {}", core_root.display());

        // 5. Family + linker script. The board's `ldscript` is relative to
        //    the framework package root (e.g.
        //    `linker_scripts/gcc/lpc845_flash.ld`); fall back to the
        //    per-family default when the board omits it.
        let lpc_family = board_lpc_family(&ctx.board)?;
        let ldscript_rel = ctx
            .board
            .ldscript
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("linker_scripts/gcc/{}_flash.ld", lpc_family));
        let linker_script_path = core.linker_script(&ldscript_rel);
        if !linker_script_path.is_file() {
            return Err(FbuildError::ConfigError(format!(
                "ArduinoCore-LPC8xx linker script not found: {} (board ldscript '{}')",
                linker_script_path.display(),
                ldscript_rel
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
            platform: "nxplpc".to_string(),
            max_flash: ctx.board.max_flash,
            max_ram: ctx.board.max_ram,
            eh_frame_policy: Some(match eh_frame_policy {
                crate::eh_frame_policy::EhFramePolicy::Strip => "strip".to_string(),
                crate::eh_frame_policy::EhFramePolicy::Preserve => "preserve".to_string(),
            }),
            extra: Some(std::collections::BTreeMap::from([(
                "lpc_family".to_string(),
                lpc_family.to_string(),
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
                        platform_label: "NXPLPC",
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

        // 6. Scan user sources, then add the vendored core sources
        //    (framework main(), startup, wiring, HardwareSerial, SPI, ...)
        //    plus the board variant glue as "core" sources.
        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let mut sources = scanner.scan_all_filtered(None, None, ctx.source_filter.as_deref())?;

        for path in collect_compilable_sources(&core.core_dir())? {
            sources.core_sources.push(path);
        }
        // The board variant.cpp pulls its base variant in via a relative
        // include, so compiling the board's translation unit is sufficient.
        let variant_cpp = core.variant_dir(&ctx.board.variant).join("variant.cpp");
        if variant_cpp.is_file() {
            sources.core_sources.push(variant_cpp);
        }

        tracing::info!(
            "sources: {} sketch, {} core (ArduinoCore-LPC8xx), {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 7. Build the per-MCU ArmMcuConfig + defines.
        let mcu_config = mcu_config::get_arm_mcu_config(lpc_family)?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());

        // 8. Include dirs: vendored core + board/base variant + CMSIS core +
        //    sketch/project discovery (libs under lib/, etc.).
        //
        //    Project-local override (FastLED/fbuild#479): when the project
        //    ships its own `variants/<variant>/pins_arduino.h` next to
        //    `platformio.ini`, that dir is prepended so its symbols win over
        //    the vendored variant default.
        let src_dir = crate::compiler::absolute_from_cwd(&ctx.src_dir);
        let project_dir_abs = crate::compiler::absolute_from_cwd(&params.project_dir);
        let mut include_dirs: Vec<PathBuf> = Vec::with_capacity(8);
        let project_variant_dir = project_dir_abs.join("variants").join(&ctx.board.variant);
        if project_variant_dir.join("pins_arduino.h").is_file() {
            tracing::info!(
                "nxplpc: using project-local variant include {}",
                project_variant_dir.display()
            );
            include_dirs.push(project_variant_dir);
            // Also expose the parent variants/ dir so that variant-chain
            // includes like `#include "../<base>/variant.h"` resolve.
            include_dirs.push(project_dir_abs.join("variants"));
        }
        include_dirs.extend([
            core.core_dir(),
            core.variant_dir(&ctx.board.variant),
            core.variant_dir(lpc_family),
            cmsis.get_core_include_dir(),
            src_dir,
        ]);
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        let lib_extra_dirs = ctx.config.get_lib_extra_dirs(&params.env_name)?;
        let extra_library_roots =
            pipeline::discover_extra_library_roots(&params.project_dir, &lib_extra_dirs);
        pipeline::add_extra_library_include_dirs(&extra_library_roots, &mut include_dirs);
        include_dirs.retain(|dir| !dir.as_os_str().is_empty());

        // 6a. Download `lib_deps` from the registry / remote URLs before
        // creating the compiler, so the downloaded library include directories
        // are available during compilation (FastLED/fbuild#1276).
        let lib_deps = ctx.config.get_lib_deps(&params.env_name)?;
        let lib_ignore = ctx
            .config
            .get_lib_ignore(&params.env_name)
            .unwrap_or_default();
        let lib_archives = if !lib_deps.is_empty() {
            let temp_compiler = ArmCompiler::new(
                toolchain.get_gcc_path(),
                toolchain.get_gxx_path(),
                lpc_family,
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

        let compiler = ArmCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            lpc_family,
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
        )
        .with_build_unflags(ctx.build_unflags.clone())
        .with_eh_frame_policy(eh_frame_policy);

        // 9. Linker. Uses the vendored per-board linker script; `-L` the
        //    framework root so the script's relative
        //    `INCLUDE linker_scripts/gcc/lpc8xx_common.ld` resolves.
        let linker = ArmLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script_path,
            mcu_config,
            params.profile,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        )
        .with_lib_search_dirs(vec![core.install_path()]);

        // 10. Compile extra library roots before the shared pipeline links them.
        //
        // Fold `ctx.user_flags` (parsed from `[env:*] build_flags`) and the
        // global compile-overlay into the library flag set so library sources
        // see the same -D defines / -std overrides the sketch will see.
        // Without this fold, the only way to get `build_flags` defines into a
        // library compile was to bake them into the board JSON's `extra_flags`
        // â€” exactly the workaround #576 installed for `lpc845brk` and that
        // this PR retires. Mirrors the ESP32 library-compile path at
        // `esp32/orchestrator/build.rs`; see FastLED/fbuild#587.
        let gcc_path = toolchain.get_gcc_path();
        let gxx_path = toolchain.get_gxx_path();
        let ar_path = toolchain.get_ar_path();
        let gcc_ar_path = toolchain.get_gcc_ar_path();
        let raw_c_flags = crate::compiler::Compiler::c_flags(&compiler);
        let raw_cpp_flags = crate::compiler::Compiler::cpp_flags(&compiler);
        // FastLED/fbuild#574: shared overlay assembly (only the user overlay is
        // needed here — the sketch/core/local-lib overlays are applied inside
        // `run_sequential_build_with_libs`).
        let (user_overlay, _src_overlay) = ctx.compile_overlays();
        let c_flags = apply_overlay_flags(&raw_c_flags, &user_overlay, "dummy.c");
        let cpp_flags = apply_overlay_flags(&raw_cpp_flags, &user_overlay, "dummy.cpp");
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
        let mut extra_link_inputs =
            pipeline::compile_extra_libraries(&extra_library_roots, &ctx.build_dir, &lib_env)
                .await?;
        extra_link_inputs.extend(lib_archives);

        // 11. Run the shared sequential build pipeline.
        let result = pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &extra_link_inputs,
            Some(&lib_env),
            TargetArchitecture::Arm,
            "NXPLPC",
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

/// Construct a boxed orchestrator for the dispatch table.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(NxpLpcOrchestrator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestrator_reports_nxplpc_platform() {
        let orch = NxpLpcOrchestrator;
        assert_eq!(orch.platform(), Platform::NxpLpc);
    }

    #[test]
    fn board_lpc_family_accepts_concrete_arduino_boards() {
        let cases = [
            ("lpc845brk", "lpc845"),
            ("lpcxpresso804", "lpc804"),
            ("lpcxpresso845max", "lpc845"),
        ];
        for (board_id, expected) in cases {
            let board = fbuild_test_support::board_for_test(board_id);
            assert_eq!(board_lpc_family(&board).unwrap(), expected);
        }
    }

    #[test]
    fn collect_compilable_sources_filters_and_sorts() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["b.cpp", "a.c", "startup.S", "header.h", "notes.txt"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let found = collect_compilable_sources(dir.path()).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // Only translation units, sorted; headers/text excluded.
        assert_eq!(names, vec!["a.c", "b.cpp", "startup.S"]);
    }

    #[test]
    fn collect_compilable_sources_errors_on_missing_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");
        assert!(collect_compilable_sources(&missing).is_err());
    }
}
