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
    pub async fn new(params: &BuildParams) -> Result<Self> {
        Self::new_with_perf(params, None).await
    }

    /// Variant that records phase timings into an optional `PerfTimer`.
    ///
    /// Orchestrators that want per-phase visibility (see [`crate::perf_log`])
    /// pass in a shared timer. Callers that don't care get zero overhead by
    /// passing `None`.
    pub async fn new_with_perf(
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
        let overlay =
            crate::script_runtime::resolve_extra_script_overlay(project_dir, env_name, &config)
                .await?;
        if let Some(p) = perf.as_mut() {
            p.record("config-parse", t0.elapsed());
        }

        // 2. Load board config
        //
        // `ResolutionContext::resolve_board` is the single board-resolution
        // entry point (FastLED/fbuild#519); it honors project-local
        // `<project_dir>/boards/<board_id>.json` discovery (FastLED/fbuild#515)
        // and `[env]` board overrides. New resolution knobs go on that
        // context, not here.
        let t0 = std::time::Instant::now();
        let board = crate::resolution::ResolutionContext::new(project_dir, env_name, &config)
            .resolve_board()?;
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
        //
        // `params.build_dir` is the authoritative env-rooted build dir
        // (caller resolves it via `fbuild_paths::BuildLayout`). We never
        // re-derive it from `project_dir` here, so callers can override
        // the layout (e.g. flatten the env segment when their project
        // dir is already named after the env — the FastLED
        // `.build/pio/<board>/` case). See FastLED/fbuild#432.
        let t0 = std::time::Instant::now();
        let build_dir = params.build_dir.clone();
        if params.clean && build_dir.exists() {
            std::fs::remove_dir_all(&build_dir)?;
        }
        let core_build_dir = build_dir.join("core");
        let src_build_dir = build_dir.join("src");
        std::fs::create_dir_all(&core_build_dir)?;
        std::fs::create_dir_all(&src_build_dir)?;
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
        let (mut user_flags, src_flags, all_src_flags) =
            apply_build_unflags(user_flags, src_flags, &build_unflags);
        // FastLED/fbuild#574: fold caller-injected one-off flags (e.g. the
        // QEMU-emulation `extra_build_flags`) into `user_flags` so they
        // propagate to framework + library + sketch compiles on EVERY
        // orchestrator. The shared sequential pipeline previously dropped them
        // (only the ESP32 path applied them), so the same build behaved
        // differently across platforms. Appended last so they intentionally
        // override board/user defaults, and NOT subject to `build_unflags`.
        user_flags.extend(params.extra_build_flags.iter().cloned());
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

    /// The `(user_overlay, src_overlay)` pair applied to every compile — the
    /// single source of truth so `[env:*] build_flags` (and caller-injected
    /// `extra_build_flags`, folded into `user_flags` at construction) reach
    /// framework/core, library, AND sketch TUs uniformly on every orchestrator.
    ///
    /// - `user_overlay` → framework/core + library compiles (`build_flags`).
    /// - `src_overlay`  → sketch + local-lib compiles (adds `build_src_flags`
    ///   and the project script overlay on top of `user_overlay`).
    ///
    /// FastLED/fbuild#574 — replaces three hand-copied overlay blocks
    /// (`pipeline::sequential`, `esp32` orchestrator, `nxplpc` orchestrator).
    pub fn compile_overlays(&self) -> (LanguageExtraFlags, LanguageExtraFlags) {
        self.compile_overlays_with_base(&self.user_flags)
    }

    /// As [`compile_overlays`](Self::compile_overlays) but with an explicit
    /// base user-flag list — the ESP32 path prepends SDK defines to
    /// `ctx.user_flags` and passes the combined list here.
    pub fn compile_overlays_with_base(
        &self,
        user_flags: &[String],
    ) -> (LanguageExtraFlags, LanguageExtraFlags) {
        assemble_compile_overlays(
            user_flags,
            &self.global_compile_overlay,
            &self.src_flags,
            &self.project_compile_overlay,
        )
    }

    /// Build the typed [`EnvNamespace`] routing key for `env_id` on `platform`
    /// — the `(env_id, platform, board, framework)` triplet from
    /// `platformio.ini [env:<id>]`. FastLED/fbuild#574. Package fetchers, cache
    /// keys, and output dirs can route off this instead of ad-hoc strings.
    pub fn env_namespace(
        &self,
        env_id: &str,
        platform: fbuild_core::Platform,
    ) -> fbuild_core::EnvNamespace {
        let (board, framework) = self
            .config
            .get_env_config(env_id)
            .map(|m| {
                (
                    m.get("board").cloned().unwrap_or_default(),
                    m.get("framework").cloned().unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        fbuild_core::EnvNamespace::new(env_id, platform, board, framework)
    }
}

/// Pure overlay assembly (unit-tested). `user_flags` (which include
/// `[env:*] build_flags` plus caller-injected `extra_build_flags`) land in BOTH
/// the user overlay (core/framework and libraries) and — via the src overlay —
/// the sketch and local libraries; `src_flags` (`build_src_flags`) land ONLY in
/// the src overlay. FastLED/fbuild#574.
fn assemble_compile_overlays(
    user_flags: &[String],
    global: &LanguageExtraFlags,
    src_flags: &[String],
    project: &LanguageExtraFlags,
) -> (LanguageExtraFlags, LanguageExtraFlags) {
    let user_overlay = LanguageExtraFlags {
        common: user_flags
            .iter()
            .cloned()
            .chain(global.common.iter().cloned())
            .collect(),
        c: global.c.clone(),
        cxx: global.cxx.clone(),
        asm: global.asm.clone(),
    };
    let src_overlay = LanguageExtraFlags::combined(&[
        &user_overlay,
        &LanguageExtraFlags {
            common: src_flags.to_vec(),
            c: Vec::new(),
            cxx: Vec::new(),
            asm: Vec::new(),
        },
        project,
    ]);
    (user_overlay, src_overlay)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_flags_reach_both_overlays_src_flags_only_src() {
        // FastLED/fbuild#574: `[env:*] build_flags` (+ folded `extra_build_flags`)
        // must reach framework/core, library AND sketch compiles; `build_src_flags`
        // only the sketch/local-lib compiles.
        let (user, src) = assemble_compile_overlays(
            &["-DENV_FLAG".to_string()],
            &LanguageExtraFlags::default(),
            &["-DSRC_ONLY".to_string()],
            &LanguageExtraFlags::default(),
        );
        assert!(
            user.common.contains(&"-DENV_FLAG".to_string()),
            "build_flags must reach the framework/library overlay"
        );
        assert!(
            !user.common.contains(&"-DSRC_ONLY".to_string()),
            "build_src_flags must NOT reach the framework overlay"
        );
        assert!(
            src.common.contains(&"-DENV_FLAG".to_string()),
            "build_flags must also reach the sketch overlay"
        );
        assert!(
            src.common.contains(&"-DSRC_ONLY".to_string()),
            "build_src_flags must reach the sketch overlay"
        );
    }
}
