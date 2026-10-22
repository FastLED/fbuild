//! Package resolution for pioarduino (platform.json, framework, toolchain).
//!
//! The build ([`resolve_pioarduino_packages`]) and `fbuild install`
//! ([`provision_esp32`]) construct packages through the same helpers, so they
//! always agree on what an env needs (FastLED/fbuild#1433).

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use fbuild_core::Result;
use fbuild_core::path::NormalizedPath;
use fbuild_core::platformio_package::{
    PackageKind as RegistryPackageKind, PackageSource, RegistrySpec, ResolvedPayload,
    parse_package_spec,
};
use sha2::{Digest, Sha256};

use super::super::mcu_config::{Esp32McuConfig, get_mcu_config};
use crate::provision::{
    PackageKind, ProvisionInputs, ProvisionMode, ProvisionStatus, ProvisionedPackage,
    provision_package,
};

/// Resolve the selected pioarduino framework and matching toolchain.
///
/// Downloads pioarduino platform.json, resolves unified toolchains via
/// metadata or older per-MCU toolchains via the PlatformIO registry, and
/// downloads the selected framework + libs packages.
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
    fbuild_packages::PackageInfo,
)> {
    // Ensure pioarduino platform (contains platform.json with metadata URLs).
    let platform = pioarduino_platform(project_dir, env_config, true)
        .await?
        .ok_or_else(|| {
            fbuild_core::FbuildError::PackageError("ESP32 platform unavailable".into())
        })?;
    fbuild_packages::Package::ensure_installed(&platform).await?;
    let platform_info = fbuild_packages::Package::get_info(&platform);
    // Resolve the exact toolchain declared by the selected platform. Older
    // pioarduino releases name per-MCU registry packages, not metadata URLs.
    let toolchain = resolve_and_create_toolchain(&platform, project_dir, mcu_config).await?;

    // Opportunistically provision any helper toolchains listed in
    // `platform.json` alongside the MCU-primary toolchain — e.g.
    // `toolchain-riscv32-esp` on ESP32-S3 (Xtensa cores + RISC-V ULP
    // coprocessor). Best-effort: a missing helper isn't fatal because most
    // FastLED sketches don't compile ULP code, but eagerly caching the
    // helper means builds that DO need it never hit a cold-cache stall.
    // See fbuild#401.
    provision_helper_toolchains(&platform, project_dir, mcu_config);

    let framework = resolve_framework(&platform, project_dir, mcu, env_config, true)
        .await?
        .ok_or_else(|| {
            fbuild_core::FbuildError::PackageError("ESP32 framework unavailable".into())
        })?;

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

    Ok((toolchain, framework, esptool_py, platform_info))
}

/// Resolve the esptool executable a deploy should spawn: the same one the
/// build uses (`FBUILD_ESPTOOL_PATH` first, then the provisioned
/// `tool-esptoolpy` package from the env's `platform.json`), so deploy works
/// from a shell with no esptool on PATH and runs the build's esptool version
/// (FastLED/fbuild#1616).
///
/// `Ok(None)` means no usable resolved esptool; the deployer then falls back
/// to a bare `esptool` PATH lookup. That includes the v4 `esptool.py` source
/// package older pioarduino pins provision: the deployer speaks v5's
/// hyphenated CLI (`write-flash`, `default-reset`), which v4 rejects.
pub async fn resolve_deploy_esptool(
    project_dir: &Path,
    env_config: Option<&HashMap<String, String>>,
) -> Result<Option<NormalizedPath>> {
    // An explicit override is the user's choice and is used as-is, whatever
    // its file name (v5 still ships `esptool.py` entry points).
    if let Some(path) = fbuild_packages::library::esptool_path_override()? {
        return Ok(Some(path));
    }
    let Some(platform) = pioarduino_platform(project_dir, env_config, true).await? else {
        return Ok(None);
    };
    fbuild_packages::Package::ensure_installed(&platform).await?;
    // The file-name check only applies to the package fbuild provisioned,
    // where `esptool.py` means the v4 source package.
    Ok(resolve_esptool(&platform, project_dir).await?.filter(|path| {
        let usable = speaks_v5_cli(path);
        if !usable {
            tracing::info!(
                "resolved esptool {} is the v4 source package; deploy uses an `esptool` on PATH",
                path.display()
            );
        }
        usable
    }))
}

/// Whether `esptool` speaks the v5 hyphenated CLI the deployer emits. The v4
/// source package installs an `esptool.py` entry point (the same signal
/// `esp32_linker` uses to pick underscore flags for `elf2image`).
pub(super) fn speaks_v5_cli(esptool: &Path) -> bool {
    !esptool
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("esptool.py"))
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

    let Some(platform) = pioarduino_platform(project_dir, env_config, mode.fetches()).await? else {
        return Ok(vec![ProvisionedPackage::new(
            PackageKind::Platform,
            "platform-espressif32",
            ProvisionStatus::WouldFetch,
        )]);
    };
    let platform_row = provision_package(PackageKind::Platform, &platform, mode).await;
    let platform_ready = is_installed(&platform_row);
    rows.push(platform_row);
    if !platform_ready {
        // Every other package is named by the platform's platform.json.
        return Ok(rows);
    }
    rows.push(provision_toolchain(&platform, project_dir, &mcu_config, mode).await);

    let Some(framework) =
        resolve_framework(&platform, project_dir, mcu, env_config, mode.fetches()).await?
    else {
        rows.push(ProvisionedPackage::new(
            PackageKind::Framework,
            "framework-arduinoespressif32",
            ProvisionStatus::WouldFetch,
        ));
        return Ok(rows);
    };
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

#[path = "packages_platform.rs"]
mod platform;
use platform::{
    ensure_sdk_libs, esptool_package, pioarduino_framework, pioarduino_platform, resolve_esptool,
    resolve_framework, sdk_libs_urls,
};

/// Provision toolchain-* packages listed in `platform.json` other than the
/// MCU-primary toolchain — e.g. `toolchain-riscv32-esp` on ESP32-S3 (Xtensa
/// cores + RISC-V ULP coprocessor). Cache-aware: a helper toolchain whose
/// cache directory already exists is skipped without touching the network,
/// so the steady-state cost on a warm cache is zero. Each resolution miss
/// is logged at warn level but never fails the caller. See fbuild#401.
#[expect(clippy::cognitive_complexity, reason = "baseline, zackees/ci.yml#229")]
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
        if !metadata_url.starts_with("https://") && !metadata_url.starts_with("http://") {
            // Older manifests declare registry versions here. The primary
            // toolchain is resolved above; optional helpers are not metadata
            // archives and must not be handed to the URL downloader.
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

/// The MCU's own toolchain package. Each Xtensa MCU has one; RISC-V MCUs
/// share the unified package and have none.
fn per_mcu_toolchain_name(mcu_config: &Esp32McuConfig) -> Option<String> {
    (!mcu_config.is_riscv()).then(|| format!("toolchain-xtensa-{}", mcu_config.mcu))
}

/// The toolchain package the platform actually declares.
///
/// Prefer a declared per-MCU package for pioarduino 51.x and official 6.x/7.x.
/// Official 1.11.2 calls ESP32's package `toolchain-xtensa32`. Pioarduino
/// 53.x/54.x declare only a unified registry package, so a missing metadata
/// URL does not imply a per-MCU package.
fn platform_toolchain_name(
    platform: &fbuild_packages::library::Esp32Platform,
    mcu_config: &Esp32McuConfig,
) -> String {
    toolchain_name_for(mcu_config, |name| platform.get_package_url(name).is_ok())
}

fn toolchain_name_for(mcu_config: &Esp32McuConfig, declares: impl Fn(&str) -> bool) -> String {
    per_mcu_toolchain_name(mcu_config)
        .filter(|name| declares(name))
        .or_else(|| {
            (mcu_config.mcu == "esp32" && declares("toolchain-xtensa32"))
                .then(|| "toolchain-xtensa32".to_string())
        })
        .unwrap_or_else(|| primary_toolchain_name(mcu_config.is_riscv()).to_string())
}

#[expect(clippy::cognitive_complexity, reason = "baseline, zackees/ci.yml#229")]
async fn resolve_and_create_toolchain(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mcu_config: &Esp32McuConfig,
) -> Result<fbuild_packages::toolchain::Esp32Toolchain> {
    let is_riscv = mcu_config.is_riscv();
    let prefix = mcu_config.toolchain_prefix();

    if !platform.has_unified_toolchain(is_riscv) {
        let name = platform_toolchain_name(platform, mcu_config);
        let requirement = platform.get_package_requirement(&name)?;
        let registry = requirement.spec.registry().ok_or_else(|| {
            fbuild_core::FbuildError::PackageError(format!(
                "{name} is not a PlatformIO registry toolchain"
            ))
        })?;
        let host =
            fbuild_core::platformio_package::host_system(fbuild_core::platform::host::current())
                .ok_or_else(|| {
                    fbuild_core::FbuildError::PackageError("unsupported PlatformIO host".into())
                })?;
        let cache_root = fbuild_packages::Cache::new(project_dir).toolchains_dir();
        let payload = match cached_registry_payload(&cache_root, registry, host)? {
            Some(payload) => payload,
            None => fbuild_packages::platformio_registry::RegistryClient::default()
                .resolve_cached(
                    registry,
                    fbuild_core::platformio_package::PackageKind::Tool,
                    host,
                    &cache_root,
                    true,
                )
                .await
                .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?
                .ok_or_else(|| {
                    fbuild_core::FbuildError::PackageError("ESP32 toolchain unavailable".into())
                })?,
        };
        cache_registry_payload(&cache_root, registry, host, &payload)?;
        tracing::info!(
            "resolved ESP32 toolchain {}@{} for {}: {} (sha256 {})",
            payload.name,
            payload.version,
            payload.system,
            payload.url,
            payload.sha256
        );
        return Ok(
            fbuild_packages::toolchain::Esp32Toolchain::from_registry_payload(
                project_dir,
                &payload,
                &prefix,
            ),
        );
    }

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
    let name = platform_toolchain_name(platform, mcu_config);
    let toolchain = if mode.fetches() {
        resolve_and_create_toolchain(platform, project_dir, mcu_config)
            .await
            .map(Some)
    } else {
        cached_toolchain(platform, project_dir, mcu_config)
    };
    match toolchain {
        Ok(Some(toolchain)) => provision_package(PackageKind::Toolchain, &toolchain, mode).await,
        Ok(None) => {
            let requested = platform
                .get_package_requirement(&name)
                .ok()
                .and_then(|entry| entry.spec.registry().cloned());
            let version = requested
                .as_ref()
                .and_then(|registry| registry.requirement.clone())
                .unwrap_or_default();
            let url = requested.map_or_else(
                || {
                    platform
                        .get_toolchain_metadata_url(is_riscv)
                        .unwrap_or_default()
                },
                |registry| {
                    format!(
                        "{}/tool/{}@{}",
                        registry.owner.as_deref().unwrap_or("<registry-owner>"),
                        registry.name,
                        registry.requirement.as_deref().unwrap_or("*")
                    )
                },
            );
            ProvisionedPackage {
                version,
                url,
                ..ProvisionedPackage::new(
                    PackageKind::Toolchain,
                    &name,
                    ProvisionStatus::WouldFetch,
                )
            }
        }
        Err(error) => ProvisionedPackage {
            error: Some(error.to_string()),
            ..ProvisionedPackage::new(PackageKind::Toolchain, &name, ProvisionStatus::Failed)
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
    if !platform.has_unified_toolchain(is_riscv) {
        let name = platform_toolchain_name(platform, mcu_config);
        let requirement = platform.get_package_requirement(&name)?;
        let registry = requirement.spec.registry().ok_or_else(|| {
            fbuild_core::FbuildError::PackageError(format!(
                "{name} is not a PlatformIO registry toolchain"
            ))
        })?;
        let host =
            fbuild_core::platformio_package::host_system(fbuild_core::platform::host::current())
                .ok_or_else(|| {
                    fbuild_core::FbuildError::PackageError("unsupported PlatformIO host".into())
                })?;
        let cache_root = fbuild_packages::Cache::new(project_dir).toolchains_dir();
        return Ok(
            cached_registry_payload(&cache_root, registry, host)?.map(|payload| {
                fbuild_packages::toolchain::Esp32Toolchain::from_registry_payload(
                    project_dir,
                    &payload,
                    &prefix,
                )
            }),
        );
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

/// Persist registry selection so `fbuild install --check` can reconstruct the
/// same toolchain offline. The archive itself remains SHA-256-verified by the
/// package installer; this sidecar is only a host-specific resolution record.
fn registry_payload_path(
    cache_root: &Path,
    registry: &RegistrySpec,
    host: &str,
) -> Result<NormalizedPath> {
    let request = serde_json::to_vec(&(registry, host)).map_err(|error| {
        fbuild_core::FbuildError::PackageError(format!("cannot encode registry request: {error}"))
    })?;
    let key = format!("{:x}", Sha256::digest(request));
    Ok(NormalizedPath::new(
        cache_root
            .join("platformio-registry-resolutions")
            .join(format!("{key}.json")),
    ))
}

fn cache_registry_payload(
    cache_root: &Path,
    registry: &RegistrySpec,
    host: &str,
    payload: &ResolvedPayload,
) -> Result<()> {
    let path = registry_payload_path(cache_root, registry, host)?;
    let directory = cache_root.join("platformio-registry-resolutions");
    std::fs::create_dir_all(&directory).map_err(|error| {
        fbuild_core::FbuildError::PackageError(format!("cannot create registry cache: {error}"))
    })?;
    let mut temporary = tempfile::NamedTempFile::new_in(&directory).map_err(|error| {
        fbuild_core::FbuildError::PackageError(format!("cannot stage registry cache: {error}"))
    })?;
    let bytes = serde_json::to_vec(payload).map_err(|error| {
        fbuild_core::FbuildError::PackageError(format!("cannot encode registry payload: {error}"))
    })?;
    temporary.write_all(&bytes).map_err(|error| {
        fbuild_core::FbuildError::PackageError(format!("cannot write registry cache: {error}"))
    })?;
    temporary.persist(&path).map_err(|error| {
        fbuild_core::FbuildError::PackageError(format!(
            "cannot publish registry cache: {}",
            error.error
        ))
    })?;
    Ok(())
}

fn cached_registry_payload(
    cache_root: &Path,
    registry: &RegistrySpec,
    host: &str,
) -> Result<Option<ResolvedPayload>> {
    let path = registry_payload_path(cache_root, registry, host)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "cannot read registry cache {}: {error}",
                path.display()
            )));
        }
    };
    let payload: ResolvedPayload = serde_json::from_slice(&bytes).map_err(|error| {
        fbuild_core::FbuildError::PackageError(format!(
            "invalid registry cache {}: {error}",
            path.display()
        ))
    })?;
    if payload.name != registry.name
        || payload.system != host
        || registry
            .owner
            .as_deref()
            .is_some_and(|owner| owner != payload.owner)
    {
        return Err(fbuild_core::FbuildError::PackageError(format!(
            "registry cache {} does not match requested package or host",
            path.display()
        )));
    }
    Ok(Some(payload))
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
///
/// `FBUILD_ESPTOOL_PATH` is checked first, as `resolve_esptool` does: a valid
/// override is the tool, and an invalid one fails the build before any
/// metadata is read.
async fn provision_esptool(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mode: ProvisionMode,
) -> Option<ProvisionedPackage> {
    let override_row =
        |status| ProvisionedPackage::new(PackageKind::Tool, "tool-esptoolpy", status);
    match fbuild_packages::library::esptool_path_override() {
        Ok(Some(path)) => {
            return Some(ProvisionedPackage {
                install_path: Some(path.display().to_string()),
                ..override_row(ProvisionStatus::Present)
            });
        }
        Err(error) => {
            return Some(ProvisionedPackage {
                error: Some(error.to_string()),
                ..override_row(ProvisionStatus::Failed)
            });
        }
        Ok(None) => {}
    }
    let requirement = platform.get_package_requirement("tool-esptoolpy").ok()?;
    let esptool = match esptool_package(&requirement, project_dir, mode.fetches()).await {
        Ok(Some(esptool)) => esptool,
        Ok(None) => return Some(override_row(ProvisionStatus::WouldFetch)),
        Err(error) => {
            return Some(ProvisionedPackage {
                error: Some(error.to_string()),
                ..override_row(ProvisionStatus::Failed)
            });
        }
    };
    let started = Instant::now();
    let mut row = ProvisionedPackage {
        version: esptool.version().to_string(),
        url: esptool.download_url().unwrap_or_default(),
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

/// Drop `lib_deps` entries the installed Arduino core bundles (`FS`,
/// `ArduinoOTA`, `ESPmDNS`, ...), exactly as the build does, so `fbuild
/// install` does not send them to the registry (FastLED/fbuild#1442).
/// Without an installed platform and framework there is nothing to compare
/// against, and every entry is returned.
pub(crate) fn downloadable_lib_deps(
    inputs: &ProvisionInputs<'_>,
    lib_deps: Vec<String>,
) -> Vec<String> {
    use fbuild_packages::{Framework as _, Package as _};
    let env_config = Some(inputs.env_config);
    if inputs
        .env_config
        .get("platform")
        .is_some_and(|value| value.contains('@'))
        || inputs
            .env_config
            .get("platform_packages")
            .is_some_and(|raw| {
                fbuild_config::parse_platform_packages_spec(raw, "framework-arduinoespressif32")
                    .ok()
                    .flatten()
                    .is_some_and(|spec| spec.registry().is_some())
            })
    {
        // Library prefiltering is synchronous. Do not inspect the default
        // pioarduino framework while a registry-pinned platform or framework
        // is selected.
        return lib_deps;
    }
    let platform_override = crate::package_override::resolve_platform_override(
        inputs.env_config,
        "platform-espressif32",
    );
    let platform = match platform_override {
        Some(override_package) => fbuild_packages::library::Esp32Platform::with_override(
            inputs.project_dir,
            override_package,
        ),
        None => fbuild_packages::library::Esp32Platform::new(inputs.project_dir),
    };
    if !platform.is_installed() {
        return lib_deps;
    }
    let framework = pioarduino_framework(
        &platform,
        inputs.project_dir,
        inputs.board.mcu.as_str(),
        env_config,
    );
    if !framework.is_installed() {
        return lib_deps;
    }
    fbuild_library_select::external_declared_deps(
        &lib_deps,
        &fbuild_packages::library::framework_library::discover_framework_libraries(
            &framework.get_libraries_dir(),
        ),
    )
}

#[cfg(test)]
mod registry_toolchain_tests {
    use super::*;
    use fbuild_core::platformio_package::{PackageKind as RegistryKind, resolve_registry_json};

    #[test]
    fn legacy_esp32s3_manifest_selects_its_per_mcu_toolchain() {
        let mcu = get_mcu_config("esp32s3").unwrap();
        assert_eq!(
            per_mcu_toolchain_name(&mcu).as_deref(),
            Some("toolchain-xtensa-esp32s3")
        );
        assert_eq!(
            primary_toolchain_name(mcu.is_riscv()),
            "toolchain-xtensa-esp-elf"
        );
        assert_eq!(mcu.toolchain_prefix(), "xtensa-esp32s3-elf-");
        assert_eq!(
            per_mcu_toolchain_name(&get_mcu_config("esp32c3").unwrap()),
            None
        );
    }

    #[test]
    fn declared_per_mcu_toolchain_wins_over_unified() {
        let s3 = get_mcu_config("esp32s3").unwrap();
        let c3 = get_mcu_config("esp32c3").unwrap();
        // official espressif32 6.x/7.x: both declared, Arduino uses per-MCU.
        let both = |n: &str| n == "toolchain-xtensa-esp32s3" || n == "toolchain-xtensa-esp-elf";
        assert_eq!(toolchain_name_for(&s3, both), "toolchain-xtensa-esp32s3");
        // pioarduino 51.x: per-MCU only.
        assert_eq!(
            toolchain_name_for(&s3, |n| n == "toolchain-xtensa-esp32s3"),
            "toolchain-xtensa-esp32s3"
        );
        // pioarduino 53.x/54.x: unified only, by registry version.
        assert_eq!(
            toolchain_name_for(&s3, |n| n == "toolchain-xtensa-esp-elf"),
            "toolchain-xtensa-esp-elf"
        );
        // RISC-V always uses the shared package.
        assert_eq!(toolchain_name_for(&c3, both), "toolchain-riscv32-esp");
    }

    #[test]
    fn registry_payload_becomes_exact_checked_toolchain_package() {
        let manifest = r#"{"packages":{"toolchain-xtensa-esp32s3":{"type":"toolchain","owner":"espressif","version":"12.2.0+20230208"}}}"#;
        let requirement =
            fbuild_core::platformio_package::resolve_platform_requirements(manifest, &[])
                .unwrap()
                .remove(0);
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let response = format!(
            "{{\"name\":\"toolchain-xtensa-esp32s3\",\"owner\":{{\"username\":\"espressif\"}},\"versions\":[{{\"name\":\"12.2.0+20230208\",\"files\":[{{\"system\":[\"linux_x86_64\"],\"download_url\":\"https://example.test/s3-gcc12.tar.gz\",\"checksum\":{{\"sha256\":\"{hash}\"}}}}]}}]}}"
        );
        let payload = resolve_registry_json(
            requirement.spec.registry().unwrap(),
            RegistryKind::Tool,
            "linux_x86_64",
            &response,
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let toolchain = fbuild_packages::toolchain::Esp32Toolchain::from_registry_payload(
            temp.path(),
            &payload,
            "xtensa-esp32s3-elf-",
        );
        let info = fbuild_packages::Package::get_info(&toolchain);
        assert_eq!(info.name, "toolchain-xtensa-esp32s3");
        assert_eq!(info.version, "12.2.0+20230208");
        assert_eq!(info.url, "https://example.test/s3-gcc12.tar.gz");
        assert_eq!(info.checksum.as_deref(), Some(hash));

        cache_registry_payload(
            temp.path(),
            requirement.spec.registry().unwrap(),
            "linux_x86_64",
            &payload,
        )
        .unwrap();
        assert_eq!(
            cached_registry_payload(
                temp.path(),
                requirement.spec.registry().unwrap(),
                "linux_x86_64"
            )
            .unwrap(),
            Some(payload)
        );
        assert!(
            cached_registry_payload(
                temp.path(),
                requirement.spec.registry().unwrap(),
                "darwin_arm64"
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn legacy_xtensa_and_riscv_registry_files_select_each_host() {
        let digest = "b".repeat(64);
        for (name, prefix) in [
            ("toolchain-xtensa-esp32s3", "xtensa-esp32s3-elf-"),
            ("toolchain-riscv32-esp", "riscv32-esp-elf-"),
        ] {
            let manifest = serde_json::json!({"packages": {
                name: {"type":"toolchain", "owner":"espressif", "version":"12.2.0+20230208"}
            }});
            let requirement = fbuild_core::platformio_package::resolve_platform_requirements(
                &manifest.to_string(),
                &[],
            )
            .unwrap()
            .remove(0);
            let response = serde_json::json!({
                "name": name,
                "owner": {"username":"espressif"},
                "versions": [{"name":"12.2.0+20230208", "files": [
                    {"system":["linux_x86_64"], "download_url":format!("https://example.test/{name}-linux.tar.gz"), "checksum":{"sha256":digest}},
                    {"system":["windows_amd64","windows_arm64"], "download_url":format!("https://example.test/{name}-windows.tar.gz"), "checksum":{"sha256":digest}},
                    {"system":["darwin_arm64"], "download_url":format!("https://example.test/{name}-macos.tar.gz"), "checksum":{"sha256":digest}}
                ]}]
            });
            for (system, suffix) in [
                ("linux_x86_64", "linux"),
                ("windows_amd64", "windows"),
                ("darwin_arm64", "macos"),
            ] {
                let payload = resolve_registry_json(
                    requirement.spec.registry().unwrap(),
                    RegistryKind::Tool,
                    system,
                    &response.to_string(),
                )
                .unwrap();
                assert_eq!(
                    payload.url,
                    format!("https://example.test/{name}-{suffix}.tar.gz")
                );
                assert_eq!(payload.sha256, digest);
                let temp = tempfile::tempdir().unwrap();
                let package = fbuild_packages::toolchain::Esp32Toolchain::from_registry_payload(
                    temp.path(),
                    &payload,
                    prefix,
                );
                assert_eq!(
                    fbuild_packages::Package::get_info(&package).version,
                    "12.2.0+20230208"
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "packages_legacy_tests.rs"]
mod legacy_tests;
