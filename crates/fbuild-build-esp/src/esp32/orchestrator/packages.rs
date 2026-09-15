//! Package resolution for pioarduino (platform.json, framework, toolchain).
//!
//! The build ([`resolve_pioarduino_packages`]) and `fbuild install`
//! ([`provision_esp32`]) construct packages through the same helpers, so they
//! always agree on what an env needs (FastLED/fbuild#1433).

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use fbuild_core::Result;
use fbuild_core::path::NormalizedPath;

use super::super::mcu_config::{Esp32McuConfig, get_mcu_config};
use crate::provision::{
    PackageKind, ProvisionInputs, ProvisionMode, ProvisionStatus, ProvisionedPackage,
    provision_package,
};

/// Resolve framework + toolchain for pioarduino mode (GCC 14 + ESP-IDF 5.x).
///
/// Downloads pioarduino platform.json, resolves toolchain via metadata,
/// and downloads the split framework + libs packages.
///
/// `env_config` is the resolved `[env:<name>]` section from `platformio.ini`,
/// used to honor `platform_packages` overrides for both
/// `platform-espressif32` and `framework-arduinoespressif32`
/// (FastLED/fbuild#672). Pass `None` if no env is in scope (cold-cache /
/// diagnostic paths) — both packages will fall back to their pinned defaults.
pub(super) async fn resolve_pioarduino_packages(
    project_dir: &Path,
    mcu: &str,
    mcu_config: &Esp32McuConfig,
    env_config: Option<&HashMap<String, String>>,
) -> Result<(
    fbuild_packages::toolchain::Esp32Toolchain,
    fbuild_packages::library::Esp32Framework,
    Option<NormalizedPath>,
)> {
    // Ensure pioarduino platform (contains platform.json with metadata URLs).
    let platform = pioarduino_platform(project_dir, env_config);
    fbuild_packages::Package::ensure_installed(&platform).await?;

    // Resolve toolchain via metadata
    let toolchain = resolve_and_create_toolchain(&platform, project_dir, mcu_config)?;

    // Opportunistically provision any helper toolchains listed in
    // `platform.json` alongside the MCU-primary toolchain — e.g.
    // `toolchain-riscv32-esp` on ESP32-S3 (Xtensa cores + RISC-V ULP
    // coprocessor). Best-effort: a missing helper isn't fatal because most
    // FastLED sketches don't compile ULP code, but eagerly caching the
    // helper means builds that DO need it never hit a cold-cache stall.
    // See fbuild#401.
    provision_helper_toolchains(&platform, project_dir, mcu_config);

    let framework = pioarduino_framework(&platform, project_dir, mcu, env_config);

    // Download the GCC toolchain (~100+ MB) CONCURRENTLY with the framework +
    // SDK libs (~hundreds of MB). Once the platform's metadata URLs are
    // resolved, these are fully independent packages (different cache dirs,
    // different install locks), so overlapping their download+extract removes
    // the smaller of the two from the critical path instead of summing them —
    // the dominant slice of the cold `pioarduino-resolve` time
    // (FastLED/fbuild#953). The framework chain stays internally ordered:
    // the framework must be installed before its SDK libs extract into
    // `tools/`.
    let (libs_url, skeleton_url) = sdk_libs_urls(&platform, mcu);

    let toolchain_fut = fbuild_packages::Package::ensure_installed(&toolchain);
    let framework_fut = async {
        fbuild_packages::Package::ensure_installed(&framework).await?;
        ensure_sdk_libs(
            &framework,
            mcu,
            libs_url.as_deref(),
            skeleton_url.as_deref(),
        )
        .await
    };
    // Provision the managed `tool-esptoolpy` package CONCURRENTLY with the
    // toolchain + framework. esptool converts firmware.elf → firmware.bin at
    // link time (FastLED/fbuild#954); provisioning it removes the pristine-
    // machine "esptool not found — pip install esptool" failure. Best-effort:
    // a resolution miss is reported at error level and returns `None`, and the
    // linker falls back to an `esptool` on PATH. Only a bad
    // `FBUILD_ESPTOOL_PATH` is fatal (FastLED/fbuild#1220).
    let esptool_fut = resolve_esptool(&platform, project_dir);

    // The three futures run to completion; surface BOTH the toolchain and
    // framework errors if both fail so the framework/SDK-libs failure (often
    // the more actionable one) isn't dropped in favor of the toolchain error
    // (CodeRabbit review on #967).
    let (toolchain_res, framework_res, esptool_res) =
        tokio::join!(toolchain_fut, framework_fut, esptool_fut);
    match (toolchain_res, framework_res) {
        (Err(tc), Err(fw)) => {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "pioarduino package resolution failed — toolchain: {tc}; framework: {fw}"
            )));
        }
        (Err(e), _) | (_, Err(e)) => return Err(e),
        (Ok(_), Ok(_)) => {}
    }
    // Checked after the toolchain/framework errors so a genuine package
    // failure still wins over a misconfigured override.
    let esptool_py = esptool_res?;

    Ok((toolchain, framework, esptool_py))
}

/// Provision what [`resolve_pioarduino_packages`] installs — platform,
/// MCU-primary toolchain, framework, SDK libs and esptool — one report row
/// each, for `fbuild install` (FastLED/fbuild#1433). Check and dry-run modes
/// resolve the toolchain from metadata already on disk and never download.
/// Helper toolchains are left out: the build only resolves their metadata and
/// never installs them.
pub(crate) async fn provision_esp32(
    inputs: &ProvisionInputs<'_>,
    mode: ProvisionMode,
) -> Result<Vec<ProvisionedPackage>> {
    let project_dir = inputs.project_dir;
    let env_config = Some(inputs.env_config);
    let mcu = inputs.board.mcu.as_str();
    let mcu_config = get_mcu_config(mcu)?;
    let mut rows = Vec::new();

    let platform = pioarduino_platform(project_dir, env_config);
    let platform_row = provision_package(PackageKind::Platform, &platform, mode).await;
    let platform_ready = is_installed(&platform_row);
    rows.push(platform_row);
    if !platform_ready {
        // Every other package is named by the platform's platform.json.
        return Ok(rows);
    }

    rows.push(provision_toolchain(&platform, project_dir, &mcu_config, mode).await);

    let framework = pioarduino_framework(&platform, project_dir, mcu, env_config);
    let framework_row = provision_package(PackageKind::Framework, &framework, mode).await;
    let framework_ready = is_installed(&framework_row);
    rows.push(framework_row);

    let (libs_url, skeleton_url) = sdk_libs_urls(&platform, mcu);
    if libs_url.is_some() || skeleton_url.is_some() {
        rows.push(
            provision_sdk_libs(
                &framework,
                framework_ready,
                mcu,
                libs_url.as_deref(),
                skeleton_url.as_deref(),
                mode,
            )
            .await,
        );
    }

    if let Some(row) = provision_esptool(&platform, project_dir, mode).await {
        rows.push(row);
    }
    Ok(rows)
}

fn is_installed(row: &ProvisionedPackage) -> bool {
    matches!(
        row.status,
        ProvisionStatus::Present | ProvisionStatus::Fetched
    )
}

/// The pioarduino platform package. Honors
/// `platform_packages = platform-espressif32@<URL>#<sha>` (FastLED/fbuild#672),
/// then `platform = <release archive URL>` (FastLED/fbuild#1432): the pin
/// replaces the const-pinned default and gets its own cache subdir via
/// `PackageBase::with_override`.
fn pioarduino_platform(
    project_dir: &Path,
    env_config: Option<&HashMap<String, String>>,
) -> fbuild_packages::library::Esp32Platform {
    let platform_ovr = env_config.and_then(|env| {
        crate::package_override::resolve_platform_override(env, "platform-espressif32")
    });
    match platform_ovr {
        Some(o) => fbuild_packages::library::Esp32Platform::with_override(project_dir, o),
        None => {
            warn_unhonored_platform_pin(env_config);
            fbuild_packages::library::Esp32Platform::new(project_dir)
        }
    }
}

/// The Arduino framework package. Override precedence (FastLED/fbuild#672):
///   1. `platform_packages = framework-arduinoespressif32@<URL>#<sha>` wins
///      outright — consumer-supplied URL replaces the platform.json-derived
///      URL and gets its own cache subdir.
///   2. Otherwise, derive the URL from platform.json.
///   3. Otherwise (very old / missing platform.json), fall back to the
///      legacy hardcoded URL via `Esp32Framework::new`.
fn pioarduino_framework(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mcu: &str,
    env_config: Option<&HashMap<String, String>>,
) -> fbuild_packages::library::Esp32Framework {
    let framework_ovr = env_config.and_then(|env| {
        crate::package_override::resolve_override(env, "framework-arduinoespressif32")
    });
    match framework_ovr {
        Some(o) => fbuild_packages::library::Esp32Framework::with_override(project_dir, o),
        None => match platform.get_package_url("framework-arduinoespressif32") {
            Ok(url) => {
                tracing::info!("resolved framework URL from platform.json");
                fbuild_packages::library::Esp32Framework::from_url(project_dir, &url)
            }
            Err(e) => {
                tracing::warn!("could not resolve framework URL, using legacy: {}", e);
                fbuild_packages::library::Esp32Framework::new(project_dir, mcu)
            }
        },
    }
}

/// URLs of the split SDK libs package (pioarduino 3.3.7+) and, for MCUs that
/// ship one, the MCU skeleton libs (e.g. ESP32-C2, ESP32-C61).
fn sdk_libs_urls(
    platform: &fbuild_packages::library::Esp32Platform,
    mcu: &str,
) -> (Option<String>, Option<String>) {
    let mcu_suffix = mcu.strip_prefix("esp32").unwrap_or("");
    let libs_url = platform
        .get_package_url("framework-arduinoespressif32-libs")
        .ok();
    let skeleton_url = if mcu_suffix.is_empty() {
        None
    } else {
        platform
            .get_package_url(&format!("framework-arduino-{}-skeleton-lib", mcu_suffix))
            .ok()
    };
    (libs_url, skeleton_url)
}

async fn ensure_sdk_libs(
    framework: &fbuild_packages::library::Esp32Framework,
    mcu: &str,
    libs_url: Option<&str>,
    skeleton_url: Option<&str>,
) -> Result<()> {
    if let Some(url) = libs_url {
        framework.ensure_libs(url, mcu).await?;
    }
    if let Some(url) = skeleton_url {
        framework.ensure_mcu_libs(url, mcu).await?;
    }
    Ok(())
}

/// Name a `platform` pin fbuild cannot honor instead of dropping it silently
/// (FastLED/fbuild#1407). Registry pins and git URLs fall back to the
/// pioarduino stable platform, which carries a different framework release.
fn warn_unhonored_platform_pin(env_config: Option<&HashMap<String, String>>) {
    let Some(value) = env_config.and_then(|env| env.get("platform")) else {
        return;
    };
    let value = value.trim();
    if value.contains('@') || value.contains("://") {
        tracing::warn!(
            "platform pin `{value}` is not a downloadable archive URL; building with the \
             pioarduino stable platform instead. Pin a release with `platform = \
             https://github.com/pioarduino/platform-espressif32/releases/download/<tag>/platform-espressif32.zip`"
        );
    }
}

/// Provision the managed `tool-esptoolpy` package (the tasmota PyInstaller
/// standalone binary) from `platform.json` and return the path to the
/// `esptool` executable.
///
/// Best-effort: a provisioning failure (missing `platform.json` entry,
/// unsupported host, network error) is reported at error level — naming the
/// parsed version and the URL that was tried — and yields `Ok(None)`, so the
/// linker's existing "esptool on PATH" fallback still applies. See
/// FastLED/fbuild#954.
///
/// The one *fatal* case is a bad `FBUILD_ESPTOOL_PATH`: an explicit override
/// that pointed nowhere used to be indistinguishable from no override at all.
/// It now fails the build immediately (FastLED/fbuild#1220).
async fn resolve_esptool(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
) -> Result<Option<NormalizedPath>> {
    // Check the override before touching platform.json: when provisioning is
    // the thing that's broken, the metadata lookup on the way to it should not
    // be able to fail the resolution first.
    if let Some(path) = fbuild_packages::library::esptool_path_override()? {
        return Ok(Some(path));
    }

    let url = match platform.get_package_url("tool-esptoolpy") {
        Ok(url) => url,
        Err(e) => {
            tracing::error!(
                "esptool provisioning failed: could not resolve tool-esptoolpy \
                 from platform.json: {e}. Falling back to an `esptool` on PATH; \
                 set {} to override.",
                fbuild_packages::library::ESPTOOL_PATH_ENV_VAR
            );
            return Ok(None);
        }
    };
    let esptool = fbuild_packages::library::Esptool::from_metadata_url(project_dir, &url);
    match esptool.ensure_installed().await {
        Ok(path) => {
            tracing::info!("provisioned esptool at {}", path.display());
            Ok(Some(path))
        }
        Err(e) => {
            // Name the parsed version and the exact URL. In #1217 the real
            // fault was a 404 on a `vunknown` URL — invisible at default
            // verbosity, and three minutes later the build died claiming
            // esptool simply wasn't installed.
            let attempted = esptool
                .download_url()
                .unwrap_or_else(|_| "<no prebuilt binary for this host>".to_string());
            tracing::error!(
                "esptool provisioning failed: {e}\n  \
                 metadata URL:  {url}\n  \
                 parsed version: {}\n  \
                 download URL:   {attempted}\n  \
                 Falling back to an `esptool` on PATH; set {} to override.",
                esptool.version(),
                fbuild_packages::library::ESPTOOL_PATH_ENV_VAR
            );
            Ok(None)
        }
    }
}

/// Provision toolchain-* packages listed in `platform.json` other than the
/// MCU-primary toolchain — e.g. `toolchain-riscv32-esp` on ESP32-S3 (Xtensa
/// cores + RISC-V ULP coprocessor). Cache-aware: a helper toolchain whose
/// cache directory already exists is skipped without touching the network,
/// so the steady-state cost on a warm cache is zero. Each resolution miss
/// is logged at warn level but never fails the caller. See fbuild#401.
fn provision_helper_toolchains(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mcu_config: &Esp32McuConfig,
) {
    let primary = primary_toolchain_name(mcu_config.is_riscv());

    let entries = match platform.enumerate_packages() {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("could not enumerate platform.json packages: {}", e);
            return;
        }
    };

    let cache = fbuild_packages::Cache::new(project_dir);
    let toolchains_dir = cache.toolchains_dir();
    for (name, metadata_url) in entries {
        if name == primary || !name.starts_with("toolchain-") {
            continue;
        }
        let cache_dir = toolchains_dir.join(&name);
        if cache_dir.exists() {
            tracing::debug!("helper toolchain {} already cached, skipping", name);
            continue;
        }
        match fbuild_packages::toolchain::esp32_metadata::resolve_toolchain_url_sync(
            &metadata_url,
            &name,
            &cache_dir,
        ) {
            Ok(resolved) => {
                tracing::info!(
                    "provisioned helper toolchain {} from platform.json: {}",
                    name,
                    resolved.url
                );
            }
            Err(e) => {
                tracing::warn!(
                    "could not provision helper toolchain {} from {}: {} \
                     (builds that don't reference this toolchain will still work)",
                    name,
                    metadata_url,
                    e
                );
            }
        }
    }
}

fn primary_toolchain_name(is_riscv: bool) -> &'static str {
    if is_riscv {
        "toolchain-riscv32-esp"
    } else {
        "toolchain-xtensa-esp-elf"
    }
}

fn resolve_and_create_toolchain(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mcu_config: &Esp32McuConfig,
) -> Result<fbuild_packages::toolchain::Esp32Toolchain> {
    let is_riscv = mcu_config.is_riscv();
    let prefix = mcu_config.toolchain_prefix();

    // Try metadata-based resolution
    match platform.get_toolchain_metadata_url(is_riscv) {
        Ok(metadata_url) => {
            let toolchain_name = primary_toolchain_name(is_riscv);

            let cache = fbuild_packages::Cache::new(project_dir);
            let cache_dir = cache.toolchains_dir().join(toolchain_name);

            match fbuild_packages::toolchain::esp32_metadata::resolve_toolchain_url_sync(
                &metadata_url,
                toolchain_name,
                &cache_dir,
            ) {
                Ok(resolved) => {
                    tracing::info!("resolved {} toolchain URL from metadata", toolchain_name);
                    Ok(fbuild_packages::toolchain::Esp32Toolchain::from_resolved(
                        project_dir,
                        &resolved.url,
                        resolved.sha256.as_deref(),
                        is_riscv,
                        &prefix,
                    ))
                }
                Err(e) => {
                    tracing::warn!("metadata resolution failed, using legacy URLs: {}", e);
                    Ok(fbuild_packages::toolchain::Esp32Toolchain::new(
                        project_dir,
                        is_riscv,
                        &prefix,
                    ))
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "could not read platform.json, using legacy toolchain URLs: {}",
                e
            );
            Ok(fbuild_packages::toolchain::Esp32Toolchain::new(
                project_dir,
                is_riscv,
                &prefix,
            ))
        }
    }
}

/// The MCU-primary toolchain row. Install resolves metadata exactly as the
/// build does; a check or dry run reads only metadata already on disk and
/// reports a would-fetch row when it has never been downloaded.
async fn provision_toolchain(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mcu_config: &Esp32McuConfig,
    mode: ProvisionMode,
) -> ProvisionedPackage {
    let is_riscv = mcu_config.is_riscv();
    let name = primary_toolchain_name(is_riscv);
    let toolchain = if mode.fetches() {
        resolve_and_create_toolchain(platform, project_dir, mcu_config).map(Some)
    } else {
        cached_toolchain(platform, project_dir, mcu_config)
    };
    match toolchain {
        Ok(Some(toolchain)) => provision_package(PackageKind::Toolchain, &toolchain, mode).await,
        Ok(None) => ProvisionedPackage {
            url: platform
                .get_toolchain_metadata_url(is_riscv)
                .unwrap_or_default(),
            ..ProvisionedPackage::new(PackageKind::Toolchain, name, ProvisionStatus::WouldFetch)
        },
        Err(error) => ProvisionedPackage {
            error: Some(error.to_string()),
            ..ProvisionedPackage::new(PackageKind::Toolchain, name, ProvisionStatus::Failed)
        },
    }
}

/// [`resolve_and_create_toolchain`] without the network: `Ok(None)` when the
/// toolchain metadata has not been downloaded yet.
fn cached_toolchain(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mcu_config: &Esp32McuConfig,
) -> Result<Option<fbuild_packages::toolchain::Esp32Toolchain>> {
    let is_riscv = mcu_config.is_riscv();
    let prefix = mcu_config.toolchain_prefix();
    if platform.get_toolchain_metadata_url(is_riscv).is_err() {
        return Ok(Some(fbuild_packages::toolchain::Esp32Toolchain::new(
            project_dir,
            is_riscv,
            &prefix,
        )));
    }
    let name = primary_toolchain_name(is_riscv);
    let cache_dir = fbuild_packages::Cache::new(project_dir)
        .toolchains_dir()
        .join(name);
    let resolved =
        fbuild_packages::toolchain::esp32_metadata::resolve_toolchain_url_cached(name, &cache_dir)?;
    Ok(resolved.map(|resolved| {
        fbuild_packages::toolchain::Esp32Toolchain::from_resolved(
            project_dir,
            &resolved.url,
            resolved.sha256.as_deref(),
            is_riscv,
            &prefix,
        )
    }))
}

/// The SDK libs row. They extract into the framework's `tools/` dir rather
/// than being a `Package`, so presence is the framework's own completeness
/// check.
async fn provision_sdk_libs(
    framework: &fbuild_packages::library::Esp32Framework,
    framework_ready: bool,
    mcu: &str,
    libs_url: Option<&str>,
    skeleton_url: Option<&str>,
    mode: ProvisionMode,
) -> ProvisionedPackage {
    let started = Instant::now();
    let url = libs_url.or(skeleton_url).unwrap_or_default();
    let mut row = ProvisionedPackage {
        version: url.rsplit('/').next().unwrap_or_default().to_string(),
        url: url.to_string(),
        ..ProvisionedPackage::new(
            PackageKind::SdkLibs,
            format!("framework-arduinoespressif32-libs ({mcu})"),
            ProvisionStatus::WouldFetch,
        )
    };
    if framework_ready && framework.sdk_libs_installed(mcu) {
        row.status = ProvisionStatus::Present;
    } else if mode.fetches() {
        if framework_ready {
            match ensure_sdk_libs(framework, mcu, libs_url, skeleton_url).await {
                Ok(()) => row.status = ProvisionStatus::Fetched,
                Err(error) => {
                    row.status = ProvisionStatus::Failed;
                    row.error = Some(error.to_string());
                }
            }
        } else {
            row.status = ProvisionStatus::Failed;
            row.error = Some("the framework is not installed".to_string());
        }
    }
    row.duration_ms = started.elapsed().as_millis() as u64;
    row
}

/// The esptool row, or `None` when `platform.json` names no esptool (the build
/// then relies on an `esptool` on PATH).
async fn provision_esptool(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mode: ProvisionMode,
) -> Option<ProvisionedPackage> {
    let metadata_url = platform.get_package_url("tool-esptoolpy").ok()?;
    let esptool = fbuild_packages::library::Esptool::from_metadata_url(project_dir, &metadata_url);
    let started = Instant::now();
    let mut row = ProvisionedPackage {
        version: esptool.version().to_string(),
        url: esptool.download_url().unwrap_or(metadata_url),
        ..ProvisionedPackage::new(
            PackageKind::Tool,
            "tool-esptoolpy",
            ProvisionStatus::WouldFetch,
        )
    };
    match esptool.installed_binary() {
        Ok(Some(binary)) => {
            row.status = ProvisionStatus::Present;
            row.install_path = Some(binary.display().to_string());
        }
        Ok(None) if mode.fetches() => match esptool.ensure_installed().await {
            Ok(binary) => {
                row.status = ProvisionStatus::Fetched;
                row.install_path = Some(binary.display().to_string());
            }
            Err(error) => {
                row.status = ProvisionStatus::Failed;
                row.error = Some(error.to_string());
            }
        },
        Ok(None) => {}
        Err(error) => {
            row.status = ProvisionStatus::Failed;
            row.error = Some(error.to_string());
        }
    }
    row.duration_ms = started.elapsed().as_millis() as u64;
    Some(row)
}
