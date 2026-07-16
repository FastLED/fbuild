//! Shared helpers used by every operation handler:
//! env-var feature switches, the RAII [`OperationGuard`], deploy-route
//! parsing, client-path resolution, and the artifact-bundle exporter.

use crate::context::DaemonContext;
use crate::models::DeployRequest;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Returns `true` when the daemon should route ESP32 `verify-flash`
/// pre-checks through the native [`espflash`] crate (issue #66) instead
/// of the Python `esptool` subprocess.
///
/// Controlled by the `FBUILD_USE_ESPFLASH_VERIFY` environment variable.
/// Native verify is enabled by default when compiled in.
/// Set this variable to `0`, `false`, `no`, or `off` (case-insensitive)
/// to force esptool.
#[cfg(feature = "espflash-native")]
pub(crate) fn native_verify_enabled() -> bool {
    env_default_enabled("FBUILD_USE_ESPFLASH_VERIFY")
}

/// Returns `true` when the daemon should trust an in-memory firmware
/// hash (keyed by port) and skip the `verify-flash` MD5 round-trip on
/// a warm redeploy.
///
/// Safety model: the hash is only honoured if the port has been
/// continuously enumerated since the hash was recorded (enforced by
/// [`DeviceManager::trusted_firmware_hash`]). If the user unplugged
/// the board and something else flashed it before it came back, the
/// disconnect edge invalidates the cached hash and the deploy falls
/// through to the normal verify-flash path.
///
/// Controlled by the `FBUILD_TRUST_DEVICE_HASH` environment variable
/// (set to `1`, `true`, `yes`, or `on` — case-insensitive). Default
/// off while the path accumulates bench time, mirroring the opt-in
/// convention used by `FBUILD_USE_ESPFLASH_{VERIFY,WRITE}`.
pub(crate) fn trust_device_hash_enabled() -> bool {
    match std::env::var("FBUILD_TRUST_DEVICE_HASH") {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

/// Compute a stable SHA-256 over the three ESP32 flash regions the
/// daemon would otherwise MD5 with `verify-flash`. Hashes the tuple
/// `(offset_le, len_le, bytes)` for each region in fixed order
/// (bootloader → partitions → firmware) so the digest uniquely
/// identifies the image that would be written; two builds of the
/// same source with identical output hash to the same value.
///
/// Memoized on [`crate::context::DaemonContext::image_hash_memo`]
/// keyed by firmware path: if all three files' `mtime` matches the
/// previously-stored tuple, the cached hash is reused (skipping the
/// 2–4 MB disk read + SHA-256) — the dominant non-serial cost on the
/// trust-skip path. Cache entries self-invalidate when any `mtime`
/// advances.
///
/// Returns `None` if any of the three files is missing on disk, so
/// the caller treats it as "can't trust-skip, fall through to
/// verify-flash."
pub(crate) fn compute_esp32_image_hash(
    ctx: &crate::context::DaemonContext,
    firmware_path: &std::path::Path,
    bootloader_offset: u32,
    partitions_offset: u32,
    firmware_offset: u32,
) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let build_dir = firmware_path.parent()?;
    let bootloader_path = build_dir.join("bootloader.bin");
    let partitions_path = build_dir.join("partitions.bin");
    let firmware = firmware_path.to_path_buf();

    let mt = |p: &std::path::Path| -> Option<std::time::SystemTime> {
        std::fs::metadata(p).ok()?.modified().ok()
    };
    let mtimes = (mt(&bootloader_path)?, mt(&partitions_path)?, mt(&firmware)?);

    // Fast path: the three files have the same `mtime` as last time
    // we hashed them, so the output bytes are unchanged. Reuse the
    // stored digest instead of re-reading + re-hashing (~5-15 ms).
    if let Some(memo) = ctx.image_hash_memo.get(&firmware) {
        if memo.bootloader_mtime == mtimes.0
            && memo.partitions_mtime == mtimes.1
            && memo.firmware_mtime == mtimes.2
        {
            return Some(memo.hash);
        }
    }

    // Miss: rebuild the digest over the current file contents and
    // record it alongside the captured `mtime`s.
    let regions: [(u32, &std::path::Path); 3] = [
        (bootloader_offset, bootloader_path.as_path()),
        (partitions_offset, partitions_path.as_path()),
        (firmware_offset, firmware.as_path()),
    ];
    let mut hasher = Sha256::new();
    for (offset, path) in &regions {
        let bytes = std::fs::read(path).ok()?;
        hasher.update(offset.to_le_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    let hash: [u8; 32] = hasher.finalize().into();
    ctx.image_hash_memo.insert(
        firmware.clone(),
        crate::context::ImageHashMemo {
            bootloader_mtime: mtimes.0,
            partitions_mtime: mtimes.1,
            firmware_mtime: mtimes.2,
            hash,
        },
    );
    Some(hash)
}

/// Returns `true` when the daemon should route ESP32 `write-flash`
/// through the native [`espflash`] crate (issue #66) instead of the
/// Python `esptool` subprocess.
///
/// Controlled by the `FBUILD_USE_ESPFLASH_WRITE` environment variable.
/// Native write is enabled by default when compiled in. Independent of
/// `FBUILD_USE_ESPFLASH_VERIFY`. Set it to `0`, `false`, `no`, or `off`
/// (case-insensitive) to force esptool.
#[cfg(feature = "espflash-native")]
pub(crate) fn native_write_enabled() -> bool {
    env_default_enabled("FBUILD_USE_ESPFLASH_WRITE")
}

#[cfg(feature = "espflash-native")]
fn env_default_enabled(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

pub(crate) fn qemu_extra_build_flags(platform: fbuild_core::Platform, mcu: &str) -> Vec<String> {
    if platform == fbuild_core::Platform::Espressif32 && mcu.eq_ignore_ascii_case("esp32s3") {
        vec![
            "-DARDUINO_USB_MODE=0".to_string(),
            "-DARDUINO_USB_CDC_ON_BOOT=0".to_string(),
        ]
    } else {
        Vec::new()
    }
}

/// RAII guard that sets `operation_in_progress` to true on creation
/// and false on drop. Also tracks daemon state and current operation description.
pub(crate) struct OperationGuard {
    ctx: Arc<DaemonContext>,
    flag: Arc<std::sync::atomic::AtomicBool>,
    state: Arc<std::sync::RwLock<fbuild_core::DaemonState>>,
    operation: Arc<std::sync::RwLock<Option<String>>>,
}

impl OperationGuard {
    pub(crate) fn new(
        ctx: &Arc<DaemonContext>,
        daemon_state: fbuild_core::DaemonState,
        description: Option<String>,
    ) -> Self {
        ctx.touch_activity();
        ctx.operation_in_progress.store(true, Ordering::Relaxed);
        if let Ok(mut s) = ctx.daemon_state.write() {
            *s = daemon_state;
        }
        if let Ok(mut op) = ctx.current_operation.write() {
            *op = description;
        }
        Self {
            ctx: Arc::clone(ctx),
            flag: Arc::clone(&ctx.operation_in_progress),
            state: Arc::clone(&ctx.daemon_state),
            operation: Arc::clone(&ctx.current_operation),
        }
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Relaxed);
        if let Ok(mut s) = self.state.write() {
            *s = fbuild_core::DaemonState::Idle;
        }
        if let Ok(mut op) = self.operation.write() {
            *op = None;
        }
        self.ctx.clear_dependency_install();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_guard_drop_clears_dependency_install_through_context() {
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        let ctx = Arc::new(DaemonContext::new(0, shutdown_tx, ".".to_string()));
        ctx.set_dependency_install(fbuild_core::install_status::status(
            "toolchain",
            Some("1.0"),
            fbuild_core::install_status::InstallPhase::WaitingForLock,
            fbuild_core::install_status::InstallRole::Waiter,
            "waiting for toolchain",
            Some(".toolchain.install.lock"),
        ));

        {
            let _guard = OperationGuard::new(
                &ctx,
                fbuild_core::DaemonState::Building,
                Some("building test".to_string()),
            );
            assert!(ctx.dependency_install_snapshot().is_some());
        }

        assert!(ctx.dependency_install_snapshot().is_none());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmulatorKind {
    Qemu,
    Avr8js,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeployRoute {
    Device,
    Emulator(EmulatorKind),
}

fn parse_emulator_kind(raw: &str) -> fbuild_core::Result<EmulatorKind> {
    match raw {
        "qemu" => Ok(EmulatorKind::Qemu),
        "avr8js" => Ok(EmulatorKind::Avr8js),
        other => Err(fbuild_core::FbuildError::DeployFailed(format!(
            "unsupported emulator '{}'",
            other
        ))),
    }
}

pub(crate) fn infer_default_emulator_kind(
    platform: fbuild_core::Platform,
    mcu: &str,
) -> Option<EmulatorKind> {
    match platform {
        fbuild_core::Platform::AtmelAvr | fbuild_core::Platform::AtmelMegaAvr => {
            Some(EmulatorKind::Avr8js)
        }
        fbuild_core::Platform::Espressif32 if mcu.eq_ignore_ascii_case("esp32s3") => {
            Some(EmulatorKind::Qemu)
        }
        _ => None,
    }
}

pub(crate) fn parse_deploy_route(
    req: &DeployRequest,
    default_emulator: Option<EmulatorKind>,
) -> fbuild_core::Result<DeployRoute> {
    if let Some(target) = req.target.as_deref() {
        return match target {
            "device" => Ok(DeployRoute::Device),
            "qemu" => Ok(DeployRoute::Emulator(EmulatorKind::Qemu)),
            "avr8js" => Ok(DeployRoute::Emulator(EmulatorKind::Avr8js)),
            other => Err(fbuild_core::FbuildError::DeployFailed(format!(
                "unsupported deploy target '{}'",
                other
            ))),
        };
    }

    let destination = req.to.as_deref().unwrap_or("device");
    match destination {
        "device" => {
            if req.qemu {
                return Err(fbuild_core::FbuildError::DeployFailed(
                    "--qemu cannot be combined with --to device".to_string(),
                ));
            }
            if let Some(emulator) = req.emulator.as_deref() {
                return Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "--emulator {} requires --to emu",
                    emulator
                )));
            }
            Ok(DeployRoute::Device)
        }
        "emu" | "emulator" => {
            let emulator = if req.qemu {
                if let Some(explicit) = req.emulator.as_deref() {
                    if explicit != "qemu" {
                        return Err(fbuild_core::FbuildError::DeployFailed(
                            "--qemu cannot be combined with a different --emulator".to_string(),
                        ));
                    }
                }
                "qemu"
            } else {
                match req.emulator.as_deref() {
                    Some(explicit) => explicit,
                    None => match default_emulator {
                        Some(EmulatorKind::Qemu) => "qemu",
                        Some(EmulatorKind::Avr8js) => "avr8js",
                        None => {
                            return Err(fbuild_core::FbuildError::DeployFailed(
                                "--to emu requires an explicit --emulator for this board"
                                    .to_string(),
                            ));
                        }
                    },
                }
            };
            Ok(DeployRoute::Emulator(parse_emulator_kind(emulator)?))
        }
        other => Err(fbuild_core::FbuildError::DeployFailed(format!(
            "unsupported deploy destination '{}'",
            other
        ))),
    }
}

pub(crate) fn resolve_client_path(
    raw: &str,
    caller_cwd: Option<&str>,
    project_dir: &Path,
) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else if let Some(cwd) = caller_cwd {
        PathBuf::from(cwd).join(path)
    } else {
        project_dir.join(path)
    }
}

/// Resolve the env-rooted build dir via [`fbuild_paths::BuildLayout`].
///
/// Centralises the override > `FBUILD_BUILD_DIR` > default precedence
/// and the env-segment auto-collapse rule (see FastLED/fbuild#432)
/// so every HTTP handler routes through the same resolver, keeping
/// the per-file LOC budget for `deploy.rs` / `build.rs` sane.
pub(crate) fn resolve_build_dir(
    build_dir_override: Option<&str>,
    flatten_env: bool,
    caller_cwd: Option<&str>,
    project_dir: &Path,
    env_name: &str,
    profile: fbuild_core::BuildProfile,
) -> PathBuf {
    let override_root = build_dir_override.map(|p| resolve_client_path(p, caller_cwd, project_dir));
    fbuild_paths::BuildLayout::new(project_dir.to_path_buf(), env_name.to_string(), profile)
        .with_override_root(override_root)
        .with_flatten_env(flatten_env)
        .resolve()
}

#[derive(Debug, Serialize)]
struct ArtifactFileEntry {
    name: String,
    role: String,
}

#[derive(Debug, Serialize)]
struct ArtifactManifest {
    platform: String,
    environment: String,
    primary_firmware: Option<String>,
    elf: Option<String>,
    files: Vec<ArtifactFileEntry>,
}

pub(crate) struct ArtifactExportResult {
    pub(crate) output_dir: PathBuf,
    pub(crate) primary_output: Option<PathBuf>,
}

fn artifact_role(name: &str, primary_firmware: Option<&Path>, elf_path: Option<&Path>) -> String {
    if primary_firmware
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == name)
    {
        "firmware".to_string()
    } else if elf_path
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == name)
    {
        "elf".to_string()
    } else {
        match name {
            "bootloader.bin" => "bootloader".to_string(),
            "partitions.bin" => "partitions".to_string(),
            "compile_commands.json" => "compile_database".to_string(),
            "symbol_analysis.txt" => "symbol_analysis".to_string(),
            _ => "artifact".to_string(),
        }
    }
}

pub(crate) fn export_artifacts_bundle(
    output_dir: &Path,
    platform: fbuild_core::Platform,
    env_name: &str,
    primary_firmware: Option<&Path>,
    elf_path: Option<&Path>,
) -> fbuild_core::Result<ArtifactExportResult> {
    std::fs::create_dir_all(output_dir)?;

    let source_dir = primary_firmware
        .and_then(|p| p.parent())
        .or_else(|| elf_path.and_then(|p| p.parent()))
        .ok_or_else(|| {
            fbuild_core::FbuildError::Other(
                "could not determine source artifact directory for export".to_string(),
            )
        })?;

    let mut copied_names = Vec::new();
    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = match path.file_name() {
            Some(name) => name,
            None => continue,
        };
        let dest = output_dir.join(file_name);
        if path != dest {
            std::fs::copy(&path, &dest)?;
        }
        copied_names.push(file_name.to_string_lossy().to_string());
    }
    copied_names.sort();
    copied_names.dedup();

    let manifest = ArtifactManifest {
        platform: format!("{:?}", platform),
        environment: env_name.to_string(),
        primary_firmware: primary_firmware
            .and_then(|p| p.file_name())
            .map(|p| p.to_string_lossy().to_string()),
        elf: elf_path
            .and_then(|p| p.file_name())
            .map(|p| p.to_string_lossy().to_string()),
        files: copied_names
            .iter()
            .map(|name| ArtifactFileEntry {
                name: name.clone(),
                role: artifact_role(name, primary_firmware, elf_path),
            })
            .collect(),
    };

    std::fs::write(
        output_dir.join("artifacts.json"),
        serde_json::to_vec_pretty(&manifest).map_err(|e| {
            fbuild_core::FbuildError::Other(format!("failed to serialize artifact manifest: {}", e))
        })?,
    )?;

    let primary_output = primary_firmware
        .and_then(|p| p.file_name())
        .map(|name| output_dir.join(name));

    Ok(ArtifactExportResult {
        output_dir: output_dir.to_path_buf(),
        primary_output,
    })
}
