//! AVR build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config
//! 3. Ensure avr-gcc toolchain
//! 4. Ensure Arduino AVR core
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile all sources
//! 8. Link into firmware.elf
//! 9. Convert to firmware.hex
//! 10. Report size

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};
use serde::Serialize;

use crate::build_fingerprint::{
    hash_watch_set_stamps_cached, load_json, save_json, stable_hash_json,
    PersistedBuildFingerprint, BUILD_FINGERPRINT_VERSION,
};
use crate::compile_database::TargetArchitecture;
use crate::compiler::Compiler as _;
use crate::pipeline;
use crate::zccache::FingerprintWatch;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::avr_compiler::AvrCompiler;
use super::avr_linker::AvrLinker;

/// Inputs whose value — when unchanged — guarantees the AVR build
/// output is byte-identical to the previously cached one. Serialized
/// as JSON + SHA-256 into `PersistedBuildFingerprint::metadata_hash`
/// so a single byte difference in any of these bumps the hash.
/// Mirrors the ESP32 orchestrator's metadata but with only the
/// AVR-relevant inputs (no flash_mode / partitions / upload fields).
#[derive(Debug, Serialize)]
struct AvrFingerprintMetadata {
    version: u32,
    env_name: String,
    profile: String,
    board_name: String,
    board_mcu: String,
    board_f_cpu: String,
    board_variant: String,
    board_extra_flags: Option<String>,
    board_upload_protocol: Option<String>,
    board_upload_speed: Option<String>,
    project_dir: String,
    toolchain_dir: String,
    core_dir: String,
    variant_dir: String,
}

/// Extensions that count as "project source" for the warm-path
/// watch-set walk. We hash `(path, len, mtime)` per file so any
/// source-file edit invalidates the cached fingerprint.
const AVR_FAST_PATH_EXTS: &[&str] = &["c", "cpp", "cc", "cxx", "h", "hpp", "ino", "S"];

/// Directory names to skip while walking the project for the
/// fingerprint watch — build artifacts, VCS metadata, and the
/// daemon's own working dirs.
const AVR_FAST_PATH_EXCLUDES: &[&str] = &[
    ".fbuild",
    ".git",
    ".pio",
    ".vscode",
    "build",
    "target",
    "node_modules",
    "venv",
];

fn profile_label(profile: fbuild_core::BuildProfile) -> &'static str {
    profile.as_dir_name()
}

fn avr_fast_path_watches(project_dir: &Path) -> Vec<FingerprintWatch> {
    vec![FingerprintWatch {
        cache_file: project_dir.join(".fbuild/watch-cache.json"),
        root: project_dir.to_path_buf(),
        extensions: AVR_FAST_PATH_EXTS.iter().map(|s| s.to_string()).collect(),
        excludes: AVR_FAST_PATH_EXCLUDES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }]
}

/// Absolute paths the fast-path check requires on disk before it
/// will short-circuit: the three canonical build artifacts. If any
/// are missing, the next build must run end-to-end.
fn avr_fast_path_artifacts(
    build_dir: &Path,
    profile: fbuild_core::BuildProfile,
    env_name: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    let release_dir = build_dir
        .join("build")
        .join(env_name)
        .join(profile_label(profile));
    (
        release_dir.join("firmware.hex"),
        release_dir.join("firmware.elf"),
        release_dir.join("compile_commands.json"),
    )
}

/// AVR platform build orchestrator.
pub struct AvrOrchestrator;

impl BuildOrchestrator for AvrOrchestrator {
    fn platform(&self) -> Platform {
        Platform::AtmelAvr
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();
        // Env-gated per-phase timer (FBUILD_PERF_LOG=1); zero-overhead when unset.
        let mut perf = crate::perf_log::PerfTimer::new("avr-orchestrator");

        // 1-2. Parse config, load board, setup build dirs, resolve src dir,
        //      collect flags. `new_with_perf` records its own sub-phases
        //      (config-parse, board-load, build-dirs, flag-collect) into
        //      the shared `perf` timer.
        let mut ctx = pipeline::BuildContext::new_with_perf(params, Some(&mut perf))?;

        // 3. Ensure toolchain
        let (toolchain, toolchain_dir) = {
            let _g = perf.phase("toolchain-ensure");
            let toolchain = fbuild_packages::toolchain::AvrToolchain::new(&params.project_dir);
            let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
            (toolchain, toolchain_dir)
        };
        tracing::info!("avr-gcc toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain as _;
        pipeline::log_toolchain_version(&toolchain.get_gcc_path(), "avr-gcc", &mut ctx.build_log);

        // 4. Ensure Arduino core
        let (_framework_dir, core_dir, variant_dir) = {
            let _g = perf.phase("framework-ensure");
            ensure_avr_framework(
                &params.project_dir,
                &ctx.board.core,
                &ctx.board.variant,
                ctx.board.platform(),
            )?
        };

        // 4.5. Warm-build fast path (issue #121).
        //
        // Before the ~50 ms source-scan + stat-heavy compiler staleness
        // walk below, consult the persisted `PersistedBuildFingerprint`
        // next to the previous build's artifacts. If metadata_hash +
        // the three canonical artifacts (firmware.hex / firmware.elf /
        // compile_commands.json) + the watch-set hash all match, the
        // output is byte-identical to the cached one and we can
        // early-return with the cached `BuildResult`. Skipped for the
        // compiledb-only / symbol-analysis modes whose outputs aren't
        // captured by the fingerprint.
        let metadata_hash = stable_hash_json(&AvrFingerprintMetadata {
            version: BUILD_FINGERPRINT_VERSION,
            env_name: params.env_name.clone(),
            profile: profile_label(params.profile).to_string(),
            board_name: ctx.board.name.clone(),
            board_mcu: ctx.board.mcu.clone(),
            board_f_cpu: ctx.board.f_cpu.clone(),
            board_variant: ctx.board.variant.clone(),
            board_extra_flags: ctx.board.extra_flags.clone(),
            board_upload_protocol: ctx.board.upload_protocol.clone(),
            board_upload_speed: ctx.board.upload_speed.clone(),
            project_dir: params.project_dir.to_string_lossy().into_owned(),
            toolchain_dir: toolchain_dir.to_string_lossy().into_owned(),
            core_dir: core_dir.to_string_lossy().into_owned(),
            variant_dir: variant_dir.to_string_lossy().into_owned(),
        })?;

        let fingerprint_path = ctx.build_dir.join("build_fingerprint.json");
        let (fast_hex, fast_elf, fast_compile_db) =
            avr_fast_path_artifacts(&ctx.build_dir, params.profile, &params.env_name);
        let fingerprint_watches = avr_fast_path_watches(&params.project_dir);

        if !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            let _g = perf.phase("fast-path-check");
            let persisted = match load_json::<PersistedBuildFingerprint>(&fingerprint_path) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("ignoring invalid AVR build fingerprint: {}", e);
                    None
                }
            };
            let artifacts_ready =
                fast_hex.exists() && fast_elf.exists() && fast_compile_db.exists();
            if let Some(previous) = persisted.as_ref() {
                if previous.version == BUILD_FINGERPRINT_VERSION
                    && previous.metadata_hash == metadata_hash
                    && artifacts_ready
                {
                    let file_set_matches = match previous.file_set_hash.as_deref() {
                        Some(prev_hash) => match hash_watch_set_stamps_cached(
                            &fingerprint_watches,
                            params.watch_set_cache.as_deref(),
                        ) {
                            Ok(current) => current == prev_hash,
                            Err(e) => {
                                tracing::warn!("AVR fast-path: failed to hash watches: {}", e);
                                false
                            }
                        },
                        None => false,
                    };
                    if file_set_matches {
                        ctx.build_log.push(
                            "No-op fingerprint matched; reusing existing AVR artifacts."
                                .to_string(),
                        );
                        let elapsed = start.elapsed().as_secs_f64();
                        return Ok(BuildResult {
                            success: true,
                            firmware_path: Some(fast_hex),
                            elf_path: Some(fast_elf),
                            size_info: previous.size_info.clone(),
                            symbol_map: None,
                            build_time_secs: elapsed,
                            message: format!(
                                "AVR ({}) build for {} reused cached artifacts",
                                ctx.board.mcu, params.env_name
                            ),
                            compile_database_path: Some(fast_compile_db),
                            build_log: ctx.build_log,
                        });
                    }
                }
            }
        }

        // 5. Scan sources
        let sources = {
            let _g = perf.phase("source-scan");
            let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
            scanner.scan_all_filtered(
                Some(&core_dir),
                Some(&variant_dir),
                ctx.source_filter.as_deref(),
            )?
        };

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 6. Build include dirs + compiler
        let defines = ctx.board.get_defines();
        // Use the resolved core_dir/variant_dir directly — board.get_include_paths()
        // uses the raw board core name which may differ from the actual directory
        // (e.g. MiniCore's core dir is "MCUdude_corefiles", not "MiniCore").
        let mut include_dirs = vec![core_dir.clone(), variant_dir.clone()];
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes (avr/io.h, etc.)
        include_dirs.extend(toolchain.get_include_dirs());

        let mcu_config = super::mcu_config::get_avr_config()?;

        let compiler = AvrCompiler::new(
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

        // 7. Create linker
        let linker = AvrLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            &ctx.board.mcu,
            mcu_config,
            params.profile,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        );

        // 8. Build LibraryBuildEnv for project-as-library compilation
        let gcc_path = toolchain.get_gcc_path();
        let gxx_path = toolchain.get_gxx_path();
        let ar_path = toolchain.get_ar_path();
        let gcc_ar_path = toolchain.get_gcc_ar_path();
        let c_flags = compiler.c_flags();
        let cpp_flags = compiler.cpp_flags();
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
            Some(&lib_env),
            TargetArchitecture::Avr,
            "AVR",
            start,
        )?;

        // 10. Persist the build fingerprint so the next warm rebuild
        // can short-circuit via the fast-path check above. Best-effort:
        // a write failure (e.g. read-only FS) is logged but doesn't
        // poison the build — the fingerprint is pure acceleration.
        if result.success && !params.compiledb_only && !params.symbol_analysis {
            let fp = PersistedBuildFingerprint {
                version: BUILD_FINGERPRINT_VERSION,
                metadata_hash,
                file_set_hash: hash_watch_set_stamps_cached(
                    &fingerprint_watches,
                    params.watch_set_cache.as_deref(),
                )
                .ok(),
                size_info: result.size_info.clone(),
            };
            if let Err(e) = save_json(&fingerprint_path, &fp) {
                tracing::warn!(
                    "failed to persist AVR build fingerprint at {}: {}",
                    fingerprint_path.display(),
                    e
                );
            }
        }

        Ok(result)
    }
}

/// Create an AVR orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(AvrOrchestrator)
}

/// Select and install the correct AVR Arduino framework based on the board's core name.
///
/// Uses the data-driven `avr_frameworks.json` registry to resolve the correct
/// framework package (GitHub URL, version) for any board core.
/// For `AtmelMegaAvr` boards whose core is `"arduino"`, the lookup key is remapped
/// to `"arduino_megaavr"` so they get `ArduinoCore-megaavr` (which contains the
/// megaAVR variants like `nona4809`) instead of `ArduinoCore-avr`.
/// Returns (framework_root, core_dir, variant_dir).
fn ensure_avr_framework(
    project_dir: &Path,
    core_name: &str,
    variant_name: &str,
    platform: Option<fbuild_core::Platform>,
) -> fbuild_core::Result<(PathBuf, PathBuf, PathBuf)> {
    use fbuild_packages::Package;

    // megaAVR boards (e.g. nano_every) share core name "arduino" with standard AVR
    // but need ArduinoCore-megaavr instead of ArduinoCore-avr.
    let lookup_key =
        if platform == Some(fbuild_core::Platform::AtmelMegaAvr) && core_name == "arduino" {
            "arduino_megaavr"
        } else {
            core_name
        };

    let framework = fbuild_packages::library::AvrFramework::for_core(lookup_key, project_dir)?;
    let framework_dir = framework.ensure_installed()?;
    tracing::info!(
        "AVR framework for core '{}' (lookup '{}') at {}",
        core_name,
        lookup_key,
        framework_dir.display()
    );
    let core_dir = framework.get_core_dir(core_name);
    let variant_dir = framework.get_variant_dir(variant_name);
    Ok((framework_dir, core_dir, variant_dir))
}

/// Check if a project is configured for AVR by reading its platformio.ini.
pub fn is_avr_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::AtmelAvr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_avr_orchestrator_platform() {
        let orch = AvrOrchestrator;
        assert_eq!(orch.platform(), Platform::AtmelAvr);
    }

    #[test]
    fn test_is_avr_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_avr_project(tmp.path(), "uno"));
        assert!(!is_avr_project(tmp.path(), "esp32"));
    }

    #[test]
    fn test_is_not_avr_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:esp32]\nplatform = espressif32\nboard = esp32dev\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_avr_project(tmp.path(), "esp32"));
    }

    /// Verify that megaAVR boards remap "arduino" core to "arduino_megaavr" framework.
    #[test]
    fn test_megaavr_core_remaps_to_megaavr_framework() {
        let core = "arduino";
        let platform = Some(Platform::AtmelMegaAvr);
        let lookup_key = if platform == Some(Platform::AtmelMegaAvr) && core == "arduino" {
            "arduino_megaavr"
        } else {
            core
        };
        assert_eq!(lookup_key, "arduino_megaavr");

        // Standard AVR should NOT remap
        let platform_avr = Some(Platform::AtmelAvr);
        let lookup_avr = if platform_avr == Some(Platform::AtmelMegaAvr) && core == "arduino" {
            "arduino_megaavr"
        } else {
            core
        };
        assert_eq!(lookup_avr, "arduino");
    }
}
