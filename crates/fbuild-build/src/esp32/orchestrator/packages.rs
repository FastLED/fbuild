//! Package resolution for pioarduino (platform.json, framework, toolchain).

use std::path::Path;

use fbuild_core::Result;

/// Resolve framework + toolchain for pioarduino mode (GCC 14 + ESP-IDF 5.x).
///
/// Downloads pioarduino platform.json, resolves toolchain via metadata,
/// and downloads the split framework + libs packages.
pub(super) async fn resolve_pioarduino_packages(
    project_dir: &Path,
    mcu: &str,
    mcu_config: &super::super::mcu_config::Esp32McuConfig,
) -> Result<(
    fbuild_packages::toolchain::Esp32Toolchain,
    fbuild_packages::library::Esp32Framework,
)> {
    // Ensure pioarduino platform (contains platform.json with metadata URLs)
    let platform = fbuild_packages::library::Esp32Platform::new(project_dir);
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

    // Resolve framework
    let framework = match platform.get_package_url("framework-arduinoespressif32") {
        Ok(url) => {
            tracing::info!("resolved framework URL from platform.json");
            fbuild_packages::library::Esp32Framework::from_url(project_dir, &url)
        }
        Err(e) => {
            tracing::warn!("could not resolve framework URL, using legacy: {}", e);
            fbuild_packages::library::Esp32Framework::new(project_dir, mcu)
        }
    };

    // Ensure framework is installed before trying to install libs
    let _ = fbuild_packages::Package::ensure_installed(&framework).await?;

    // Ensure SDK libs (split package in pioarduino 3.3.7+)
    if let Ok(libs_url) = platform.get_package_url("framework-arduinoespressif32-libs") {
        framework.ensure_libs(&libs_url).await?;
    }

    // Ensure MCU-specific skeleton libs (e.g. ESP32-C2, ESP32-C61).
    // Some MCUs ship their SDK in a separate skeleton package.
    let mcu_suffix = mcu.strip_prefix("esp32").unwrap_or("");
    if !mcu_suffix.is_empty() {
        let skeleton_name = format!("framework-arduino-{}-skeleton-lib", mcu_suffix);
        if let Ok(skeleton_url) = platform.get_package_url(&skeleton_name) {
            framework.ensure_mcu_libs(&skeleton_url, mcu).await?;
        }
    }

    Ok((toolchain, framework))
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
    mcu_config: &super::super::mcu_config::Esp32McuConfig,
) {
    let primary = if mcu_config.is_riscv() {
        "toolchain-riscv32-esp"
    } else {
        "toolchain-xtensa-esp-elf"
    };

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

fn resolve_and_create_toolchain(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mcu_config: &super::super::mcu_config::Esp32McuConfig,
) -> Result<fbuild_packages::toolchain::Esp32Toolchain> {
    let is_riscv = mcu_config.is_riscv();
    let prefix = mcu_config.toolchain_prefix();

    // Try metadata-based resolution
    match platform.get_toolchain_metadata_url(is_riscv) {
        Ok(metadata_url) => {
            let toolchain_name = if is_riscv {
                "toolchain-riscv32-esp"
            } else {
                "toolchain-xtensa-esp-elf"
            };

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
