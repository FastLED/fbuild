//! `BuildContext`: common build state initialized at the start of every
//! platform's `build()` method.

use std::path::PathBuf;

use fbuild_core::{BuildLog, Result};

use crate::flag_overlay::LanguageExtraFlags;
use crate::BuildParams;

use super::build_unflags::{apply_build_unflags, apply_debug_build_type, remove_unflagged_tokens};

/// Common build state initialized at the start of every platform's `build()` method.
///
/// Created by [`BuildContext::new()`], which handles config parsing, board loading,
/// build directory setup, source directory resolution, and user flag collection.
pub struct BuildContext {
    pub config: fbuild_config::PlatformIOConfig,
    pub board: fbuild_config::BoardConfig,
    pub build_log: BuildLog,
    pub build_dir: PathBuf,
    pub core_build_dir: PathBuf,
    pub src_build_dir: PathBuf,
    pub src_dir: PathBuf,
    pub source_filter: Option<String>,
    pub user_flags: Vec<String>,
    pub src_flags: Vec<String>,
    pub all_src_flags: Vec<String>,
    pub global_compile_overlay: LanguageExtraFlags,
    pub project_compile_overlay: LanguageExtraFlags,
    pub overlay_link_flags: Vec<String>,
    pub overlay_link_libs: Vec<String>,
    /// Tokens from PlatformIO `build_unflags` to strip from the effective
    /// compile command. Already applied to `user_flags` / `src_flags` /
    /// `overlay_link_flags` by `BuildContext::new`; orchestrators can pass
    /// this to their platform compiler (via e.g. `with_build_unflags`) to
    /// also filter framework/toolchain-contributed flags. See
    /// FastLED/fbuild#37.
    pub build_unflags: Vec<String>,
}

impl BuildContext {
    /// Parse platformio.ini, load board config, setup build directories,
    /// resolve source directory, and collect user flags.
    ///
    /// Takes `&BuildParams` so that new fields (e.g. `src_dir`) flow through
    /// automatically — orchestrators just pass `params` without listing every field.
    pub fn new(params: &BuildParams) -> Result<Self> {
        Self::new_with_perf(params, None)
    }

    /// Variant that records phase timings into an optional `PerfTimer`.
    ///
    /// Orchestrators that want per-phase visibility (see [`crate::perf_log`])
    /// pass in a shared timer. Callers that don't care get zero overhead by
    /// passing `None`.
    pub fn new_with_perf(
        params: &BuildParams,
        mut perf: Option<&mut crate::perf_log::PerfTimer>,
    ) -> Result<Self> {
        let project_dir = &params.project_dir;
        let env_name = &params.env_name;

        // 1. Parse platformio.ini, attaching any forwarded `PLATFORMIO_*` env
        // var overrides from the CLI caller (the daemon does not inherit
        // caller env vars).
        let t0 = std::time::Instant::now();
        let ini_path = project_dir.join("platformio.ini");
        let pio_overrides = fbuild_config::PioEnvOverrides::from_map(params.pio_env.clone());
        let config =
            fbuild_config::PlatformIOConfig::from_path_with_overrides(&ini_path, pio_overrides)?;
        let env_config = config.get_env_config(env_name)?;
        let overlay =
            crate::script_runtime::resolve_extra_script_overlay(project_dir, env_name, &config)?;
        if let Some(p) = perf.as_mut() {
            p.record("config-parse", t0.elapsed());
        }

        // 2. Load board config
        let t0 = std::time::Instant::now();
        let board_id = env_config.get("board").ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError("missing 'board' in environment config".into())
        })?;
        let overrides = config.get_board_overrides(env_name)?;
        let board = fbuild_config::BoardConfig::from_board_id(board_id, &overrides)?;
        if let Some(p) = perf.as_mut() {
            p.record("board-load", t0.elapsed());
        }

        // 3. Build log initialization
        let mut build_log = if params.no_timestamp {
            crate::build_output::create_build_log(params.log_sender.clone())
        } else {
            crate::build_output::create_build_log_with_epoch(
                params.log_sender.clone(),
                std::time::Instant::now(),
            )
        };
        crate::build_output::log_build_banner(&mut build_log, env_name);
        crate::build_output::log_board_info(
            &mut build_log,
            &board.name,
            &board.mcu,
            &board.f_cpu,
            board.max_flash,
            board.max_ram,
        );
        for note in &overlay.notes {
            build_log.push(format!("extra_scripts: {}", note));
        }

        // 4. Setup build directories
        let t0 = std::time::Instant::now();
        let cache = fbuild_packages::Cache::new(project_dir);
        if params.clean {
            cache.clean_build(env_name, params.profile)?;
        }
        cache.ensure_build_directories(env_name, params.profile)?;

        let build_dir = cache.get_build_dir(env_name, params.profile);
        let core_build_dir = cache.get_core_build_dir(env_name, params.profile);
        let src_build_dir = cache.get_src_build_dir(env_name, params.profile);
        if let Some(p) = perf.as_mut() {
            p.record("build-dirs", t0.elapsed());
        }

        // 5. Resolve source directory (Arduino IDE convention: fall back to project root)
        // Priority: explicit override (from HTTP request) > env var > INI config > "src"
        let src_dir = project_dir.join(
            params
                .src_dir
                .as_deref()
                .map(|s| s.to_string())
                .or_else(|| config.get_src_dir(env_name).ok().flatten())
                .unwrap_or_else(|| "src".to_string()),
        );
        let src_dir = if src_dir.exists() {
            src_dir
        } else {
            project_dir.to_path_buf()
        };
        let source_filter = config.get_source_filter(env_name)?;

        // 6. Collect user flags
        let t0 = std::time::Instant::now();
        let build_type = config.get_build_type(env_name)?;
        let user_flags = config.get_build_flags(env_name)?;
        crate::warn_debug_build_flags(&user_flags);
        let src_flags = config.get_build_src_flags(env_name)?;
        let overlay_link_flags = overlay.link.flags.clone();
        let (user_flags, src_flags, mut overlay_link_flags) = if build_type == "debug" {
            let debug_build_flags = config.get_debug_build_flags(env_name)?;
            apply_debug_build_type(
                user_flags,
                src_flags,
                overlay_link_flags,
                &debug_build_flags,
            )
        } else {
            (user_flags, src_flags, overlay_link_flags)
        };
        let build_unflags = config.get_build_unflags(env_name)?;
        let (user_flags, src_flags, all_src_flags) =
            apply_build_unflags(user_flags, src_flags, &build_unflags);
        remove_unflagged_tokens(&mut overlay_link_flags, &build_unflags);
        if let Some(p) = perf.as_mut() {
            p.record("flag-collect", t0.elapsed());
        }

        Ok(Self {
            config,
            board,
            build_log,
            build_dir,
            core_build_dir,
            src_build_dir,
            src_dir,
            source_filter,
            user_flags,
            src_flags,
            all_src_flags,
            global_compile_overlay: overlay.global_compile,
            project_compile_overlay: overlay.project_compile,
            overlay_link_flags,
            overlay_link_libs: overlay.link.libs,
            build_unflags,
        })
    }
}
