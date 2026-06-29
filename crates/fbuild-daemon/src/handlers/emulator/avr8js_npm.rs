//! Manage the cached `node_modules/avr8js` install used by the headless AVR8js
//! runner. Detects corrupt/partial installs, optionally force-refreshes (env
//! `FBUILD_REFRESH_EMU_CACHE=1`), and surfaces actionable errors when `node` /
//! `npm` aren't on PATH.

use std::path::{Path, PathBuf};

pub(crate) async fn find_node() -> fbuild_core::Result<PathBuf> {
    let node = if cfg!(windows) { "node.exe" } else { "node" };
    // Route through fbuild-core's `run_command` so the probe spawn is
    // captured by the daemon's containment group (issue #32). The probe
    // is short-lived (`node --version`) but a missing binary should
    // still bubble up the same way.
    match fbuild_core::subprocess::run_command(&[node, "--version"], None, None, None).await {
        Ok(output) if output.success() => Ok(PathBuf::from(node)),
        _ => Err(fbuild_core::FbuildError::DeployFailed(
            "Node.js is required for headless avr8js emulation but 'node' was not found on PATH. \
             Install Node.js 18+ from https://nodejs.org/"
                .to_string(),
        )),
    }
}

/// Env-var that forces `ensure_avr8js_npm` to wipe and reinstall the cache
/// regardless of the integrity marker. Set `FBUILD_REFRESH_EMU_CACHE=1` before
/// invoking `fbuild test-emu` (or restart the daemon with it in the env) to
/// recover from a corrupt or partial avr8js install.
pub(crate) const REFRESH_EMU_CACHE_ENV: &str = "FBUILD_REFRESH_EMU_CACHE";

pub(crate) fn refresh_emu_cache_requested() -> bool {
    matches!(
        std::env::var(REFRESH_EMU_CACHE_ENV)
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

pub(crate) fn avr8js_cache_is_intact(cache_dir: &Path) -> bool {
    // Cheapest proof of a non-corrupt install: both the `avr8js` module dir
    // *and* its `package.json` must exist. A stale/partial `npm install`
    // sometimes leaves the directory without the manifest and causes
    // Node to fail with `ERR_MODULE_NOT_FOUND` at runtime.
    let module_dir = cache_dir.join("node_modules").join("avr8js");
    let marker = module_dir.join("package.json");
    module_dir.is_dir() && marker.is_file()
}

/// Describes what `prepare_avr8js_cache_for_install` did to the cache dir
/// before a reinstall attempt. Exposed for unit testing; production code
/// only needs the side-effect on disk.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Avr8jsCachePrep {
    /// Cache was already intact — no-op, reinstall not required.
    AlreadyIntact,
    /// No preexisting cache tree; reinstall will populate a fresh dir.
    NothingToClean,
    /// `node_modules/` was present but corrupt (missing marker); wiped.
    WipedNodeModules,
    /// Force-refresh was requested; entire `cache_dir` was wiped.
    ForceWiped,
}

/// Inspect `cache_dir`, wipe corrupt/partial installs, and return what was
/// done. Does NOT run `npm install`.
pub(crate) fn prepare_avr8js_cache_for_install(
    cache_dir: &Path,
    force_refresh: bool,
) -> Avr8jsCachePrep {
    if !force_refresh && avr8js_cache_is_intact(cache_dir) {
        return Avr8jsCachePrep::AlreadyIntact;
    }

    if force_refresh && cache_dir.exists() {
        tracing::info!(
            "{}=1 set; wiping avr8js cache at {}",
            REFRESH_EMU_CACHE_ENV,
            cache_dir.display()
        );
        if let Err(e) = std::fs::remove_dir_all(cache_dir) {
            tracing::warn!(
                "failed to wipe avr8js cache at {}: {} (continuing with reinstall)",
                cache_dir.display(),
                e
            );
        }
        return Avr8jsCachePrep::ForceWiped;
    }

    if cache_dir.exists() {
        let node_modules = cache_dir.join("node_modules");
        if node_modules.exists() {
            tracing::warn!(
                "avr8js cache at {} is corrupt (missing node_modules/avr8js/package.json); reinstalling",
                cache_dir.display()
            );
            if let Err(e) = std::fs::remove_dir_all(&node_modules) {
                tracing::warn!(
                    "failed to wipe avr8js node_modules at {}: {} (continuing with reinstall)",
                    node_modules.display(),
                    e
                );
            }
            return Avr8jsCachePrep::WipedNodeModules;
        }
    }

    Avr8jsCachePrep::NothingToClean
}

pub(crate) async fn ensure_avr8js_npm() -> fbuild_core::Result<PathBuf> {
    let cache_dir = fbuild_paths::get_cache_root().join("avr8js-node");
    ensure_avr8js_npm_in(&cache_dir, refresh_emu_cache_requested()).await?;
    Ok(cache_dir)
}

/// Populate `cache_dir` with a fresh `node_modules/avr8js` install, wiping
/// a corrupt or partial install as needed. Split out from `ensure_avr8js_npm`
/// so unit tests can inject a temporary cache dir without touching env vars.
pub(crate) async fn ensure_avr8js_npm_in(
    cache_dir: &Path,
    force_refresh: bool,
) -> fbuild_core::Result<()> {
    // `prepare_avr8js_cache_for_install` is sync (tested from sync contexts)
    // and may perform `remove_dir_all` on a large `node_modules/` tree.
    // Push the blocking I/O off the async runtime via `spawn_blocking` so
    // we don't stall other handlers while a corrupt cache is wiped.
    let cache_dir_owned = cache_dir.to_path_buf();
    let prep = tokio::task::spawn_blocking(move || {
        prepare_avr8js_cache_for_install(&cache_dir_owned, force_refresh)
    })
    .await
    .map_err(|e| {
        fbuild_core::FbuildError::DeployFailed(format!(
            "avr8js cache prep task failed to join: {}",
            e
        ))
    })?;
    if prep == Avr8jsCachePrep::AlreadyIntact {
        return Ok(());
    }

    tokio::fs::create_dir_all(cache_dir).await.map_err(|e| {
        fbuild_core::FbuildError::DeployFailed(format!(
            "failed to create avr8js cache dir at {}: {}",
            cache_dir.display(),
            e
        ))
    })?;

    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    // Route through `run_command` (which spawns via the daemon's
    // containment group) so an `npm install` killed mid-flight doesn't
    // leak node processes after the daemon dies. See FastLED/fbuild#32.
    let cache_dir_str = cache_dir.to_string_lossy().to_string();
    let output = fbuild_core::subprocess::run_command(
        &[
            npm,
            "install",
            "--save",
            "avr8js@0.21.0",
            "--prefix",
            &cache_dir_str,
        ],
        None,
        None,
        None,
    )
    .await
    .map_err(|e| {
        fbuild_core::FbuildError::DeployFailed(format!(
            "failed to launch 'npm' (for `npm install avr8js@0.21.0 --prefix {}`): {}. \
             Ensure `npm` is installed alongside Node.js and on PATH \
             (https://nodejs.org/). If npm is installed, set \
             {}=1 to force a clean reinstall.",
            cache_dir.display(),
            e,
            REFRESH_EMU_CACHE_ENV
        ))
    })?;
    if !output.success() {
        return Err(fbuild_core::FbuildError::DeployFailed(format!(
            "`npm install avr8js@0.21.0 --prefix {}` exited with status {}.\n\
             --- stdout ---\n{}\n--- stderr ---\n{}",
            cache_dir.display(),
            output.exit_code,
            output.stdout.trim_end(),
            output.stderr.trim_end()
        )));
    }

    // Post-install integrity check: guard against npm exiting 0 without
    // actually extracting the package (rare, but has been seen on Windows
    // under antivirus interference).
    if !avr8js_cache_is_intact(cache_dir) {
        return Err(fbuild_core::FbuildError::DeployFailed(format!(
            "avr8js npm install at {} reported success but the cache is still \
             incomplete (missing node_modules/avr8js/package.json). \
             Try rerunning with {}=1.",
            cache_dir.display(),
            REFRESH_EMU_CACHE_ENV
        )));
    }

    Ok(())
}
