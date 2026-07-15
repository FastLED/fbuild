//! RP2040/RP2350 deployment through the stock UF2 BOOTSEL transports.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fbuild_core::{FbuildError, Result};

use crate::{DeployOutcome, Deployer, DeploymentResult};

#[path = "rp2040_target.rs"]
mod target;
#[path = "rp2040_mount.rs"]
mod mount;
#[path = "rp2040_picotool.rs"]
mod picotool;
use mount::try_mount_linux_rom_device;
use target::{
    resolve_requested_runtime_target, select_cdc_candidate, serial_selector,
};

const UF2_MAGIC_START0: u32 = 0x0A32_4655;
const UF2_MAGIC_START1: u32 = 0x9E5D_5157;
const UF2_MAGIC_END: u32 = 0x0AB1_6F30;
const UF2_FLAG_FAMILY_ID_PRESENT: u32 = 0x0000_2000;
const RP2040_FAMILY_ID: u32 = 0xE48B_FF56;
const RP2350_FAMILY_ID: u32 = 0xE48B_FF59;
// fbuild's firmware.bin is the complete flash image: it starts with the
// second-stage bootloader at the RP2040 XIP address 0x1000_0000. The 0x2000
// default used by Arduino-Pico's uf2conv.py is only for app-only BINs whose
// boot2 has already been stripped. Encoding this full image at 0x2000 leaves
// stock ROM BOOTSEL in place after an apparently successful copy.
const RP2040_UF2_BASE_ADDRESS: u32 = 0x1000_0000;
const UF2_PAYLOAD_SIZE: usize = 256;
const UF2_BLOCK_SIZE: usize = 512;

/// Build UF2 blocks for a raw RP2040 flash image.
pub fn encode_uf2(binary: &[u8]) -> Vec<u8> {
    encode_uf2_at_address(binary, RP2040_UF2_BASE_ADDRESS, RP2040_FAMILY_ID)
}

/// Build UF2 blocks using an explicit Raspberry Pi family identifier.
pub fn encode_uf2_for_family(binary: &[u8], family_id: u32) -> Vec<u8> {
    encode_uf2_at_address(binary, RP2040_UF2_BASE_ADDRESS, family_id)
}

fn encode_uf2_at_address(binary: &[u8], base_address: u32, family_id: u32) -> Vec<u8> {
    let block_count = binary.len().div_ceil(UF2_PAYLOAD_SIZE).max(1);
    let mut output = Vec::with_capacity(block_count * UF2_BLOCK_SIZE);
    for block_no in 0..block_count {
        let start = block_no * UF2_PAYLOAD_SIZE;
        let end = (start + UF2_PAYLOAD_SIZE).min(binary.len());
        let payload_len = end.saturating_sub(start);
        let mut block = [0u8; UF2_BLOCK_SIZE];
        put_u32(&mut block, 0, UF2_MAGIC_START0);
        put_u32(&mut block, 4, UF2_MAGIC_START1);
        put_u32(&mut block, 8, UF2_FLAG_FAMILY_ID_PRESENT);
        put_u32(&mut block, 12, start as u32 + base_address);
        // UF2 blocks always advertise a full 256-byte payload. The final
        // block is zero-padded; advertising its short source length makes
        // the ROM BOOTSEL parser reject an otherwise valid image.
        put_u32(&mut block, 16, UF2_PAYLOAD_SIZE as u32);
        put_u32(&mut block, 20, block_no as u32);
        put_u32(&mut block, 24, block_count as u32);
        put_u32(&mut block, 28, family_id);
        block[32..32 + payload_len].copy_from_slice(&binary[start..end]);
        put_u32(&mut block, 508, UF2_MAGIC_END);
        output.extend_from_slice(&block);
    }
    output
}

fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Return candidate removable roots for this host. Kept separate from the
/// marker check so unit tests can use a temporary directory on every OS.
fn volume_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if cfg!(windows) {
        for letter in b'A'..=b'Z' {
            roots.push(PathBuf::from(format!("{}:\\", letter as char)));
        }
    } else {
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            if let Some(user) = home.file_name() {
                roots.push(PathBuf::from("/media").join(user));
                roots.push(PathBuf::from("/run/media").join(user));
            }
        }
        roots.push(PathBuf::from("/Volumes"));
        roots.push(PathBuf::from("/media"));
        roots.push(PathBuf::from("/run/media"));
    }
    roots
}

fn has_uf2_marker(path: &Path) -> bool {
    let info = path.join("INFO_UF2.TXT");
    if let Ok(contents) = fs::read_to_string(info) {
        let upper = contents.to_ascii_uppercase();
        return upper.contains("RP2") || upper.contains("RP2040") || upper.contains("RP2350");
    }
    false
}

/// Find a mounted Pico BOOTSEL volume under the supplied roots.
pub fn find_uf2_volume(roots: &[PathBuf]) -> Option<PathBuf> {
    find_uf2_volumes(roots).into_iter().next()
}

fn find_uf2_volumes(roots: &[PathBuf]) -> Vec<PathBuf> {
    // Linux roots intentionally overlap (`/media/$USER` and `/media`) so
    // discovery still works when HOME is unavailable. Deduplicate paths
    // before applying the multi-board safety check.
    let mut matches = BTreeSet::new();
    for root in roots {
        if has_uf2_marker(root) {
            matches.insert(root.clone());
        }
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && has_uf2_marker(&path) {
                matches.insert(path);
            }
        }
    }
    matches.into_iter().collect()
}

fn find_uf2_volume_until(timeout: Duration) -> Result<Option<PathBuf>> {
    let deadline = Instant::now() + timeout;
    let mut mount_attempted = false;
    loop {
        if let Some(path) = select_single_uf2_volume(find_uf2_volumes(&volume_roots()))? {
            return Ok(Some(path));
        }
        if !mount_attempted {
            mount_attempted = try_mount_linux_rom_device();
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn select_single_uf2_volume(mut matches: Vec<PathBuf>) -> Result<Option<PathBuf>> {
    if matches.len() > 1 {
        return Err(FbuildError::DeployFailed(format!(
            "found multiple RP2040 BOOTSEL volumes: {}; pass an explicit UF2 volume path to select one",
            matches
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(matches.pop())
}

fn explicit_uf2_volume(selector: &str) -> Option<PathBuf> {
    let candidate = selector
        .strip_prefix("UF2=")
        .or_else(|| selector.strip_prefix("uf2="))
        .unwrap_or(selector);
    let path = PathBuf::from(candidate);
    has_uf2_marker(&path).then_some(path)
}

fn touch_1200bps(port: &str) -> Result<()> {
    match serialport::new(port, 9600)
        .timeout(Duration::from_secs(2))
        .open()
    {
        Ok(mut serial) => {
            // Match Arduino-Pico's reset handshake: assert/deassert DTR,
            // switch to the magic baud, then close the handle.
            serial.write_data_terminal_ready(true).map_err(|error| {
                FbuildError::SerialError(format!(
                    "failed to assert DTR while resetting {port}: {error}"
                ))
            })?;
            serial.write_data_terminal_ready(false).map_err(|error| {
                FbuildError::SerialError(format!(
                    "failed to deassert DTR while resetting {port}: {error}"
                ))
            })?;
            serial.set_baud_rate(1200).map_err(|error| {
                FbuildError::SerialError(format!(
                    "failed to set the RP2040 reset baud on {port}: {error}"
                ))
            })?;
            std::thread::sleep(Duration::from_millis(100));
            Ok(())
        }
        Err(error) if error.kind() == serialport::ErrorKind::NoDevice => Ok(()),
        Err(error) => Err(FbuildError::SerialError(format!(
            "failed to enter RP2040 BOOTSEL on {port}: {error}"
        ))),
    }
}

fn prepare_uf2_artifact(firmware_path: &Path, family_id: u32) -> Result<PathBuf> {
    let input_is_uf2 = firmware_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("uf2"));
    let input_is_bin = firmware_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("bin"));
    if !input_is_uf2 && !input_is_bin {
        return Err(FbuildError::DeployFailed(format!(
            "unsupported RP2040 firmware input {}; expected a managed .uf2 or raw .bin artifact",
            firmware_path.display()
        )));
    }
    let input_bytes = fs::read(firmware_path).map_err(|error| {
        FbuildError::DeployFailed(format!(
            "failed to read {}: {error}",
            firmware_path.display()
        ))
    })?;
    let sibling_uf2 = firmware_path.with_extension("uf2");
    let (artifact, uf2) = if input_is_uf2 {
        reject_stale_uf2(firmware_path)?;
        (firmware_path.to_path_buf(), input_bytes)
    } else if sibling_uf2.is_file() {
        reject_stale_uf2(&sibling_uf2)?;
        let bytes = fs::read(&sibling_uf2).map_err(|error| {
            FbuildError::DeployFailed(format!(
                "failed to read managed RP2040 UF2 {}: {error}",
                sibling_uf2.display()
            ))
        })?;
        (sibling_uf2, bytes)
    } else {
        let bytes = encode_uf2_for_family(&input_bytes, family_id);
        let artifact = firmware_path.with_extension("uf2");
        fs::write(&artifact, &bytes).map_err(|error| {
            FbuildError::DeployFailed(format!(
                "failed to write RP2040 UF2 artifact {}: {error}",
                artifact.display()
            ))
        })?;
        (artifact, bytes)
    };
    validate_uf2(&uf2, family_id)?;

    Ok(artifact)
}

fn copy_prepared_uf2(artifact: &Path, volume: &Path) -> Result<PathBuf> {
    let destination = volume.join("NEW.UF2");
    copy_uf2_artifact(artifact, &destination, volume)?;
    Ok(destination)
}

#[cfg(test)]
fn write_uf2(firmware_path: &Path, volume: &Path, family_id: u32) -> Result<PathBuf> {
    let artifact = prepare_uf2_artifact(firmware_path, family_id)?;
    copy_prepared_uf2(&artifact, volume)
}

fn reject_stale_uf2(uf2: &Path) -> Result<()> {
    let uf2_modified = fs::metadata(uf2)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| {
            FbuildError::DeployFailed(format!(
                "failed to inspect RP2040 UF2 artifact {}: {error}",
                uf2.display()
            ))
        })?;
    for extension in ["elf", "bin"] {
        let source = uf2.with_extension(extension);
        let Ok(source_modified) = fs::metadata(&source).and_then(|metadata| metadata.modified())
        else {
            continue;
        };
        if source_modified > uf2_modified {
            return Err(FbuildError::DeployFailed(format!(
                "RP2040 UF2 {} is older than {}; rebuild without --skip-build before deploying",
                uf2.display(),
                source.display()
            )));
        }
    }
    Ok(())
}

fn copy_uf2_artifact(artifact: &Path, destination: &Path, volume: &Path) -> Result<()> {
    copy_uf2_artifact_with(artifact, destination, volume, |source, target| {
        fs::copy(source, target)
    })
}

fn copy_uf2_artifact_with<F>(
    artifact: &Path,
    destination: &Path,
    volume: &Path,
    copy: F,
) -> Result<()>
where
    F: FnOnce(&Path, &Path) -> std::io::Result<u64>,
{
    match copy(artifact, destination) {
        Ok(_) => Ok(()),
        Err(copy_error) => {
            // Some hosts report a final I/O error when the ROM ejects the
            // virtual FAT volume immediately after accepting the last block.
            // Disappearance is the ROM's positive transition signal; if the
            // marker remains, preserve the original actionable copy error.
            if is_device_disappearance_error(&copy_error)
                && wait_for_volume_disappearance(volume, Duration::from_secs(2)).is_ok()
            {
                tracing::debug!(
                    error = %copy_error,
                    volume = %volume.display(),
                    "RP2040 volume ejected while the host finalized NEW.UF2; treating transfer as accepted"
                );
                Ok(())
            } else {
                Err(FbuildError::DeployFailed(format!(
                    "failed to copy RP2040 UF2 {} to {}: {copy_error}",
                    artifact.display(),
                    destination.display()
                )))
            }
        }
    }
}

fn is_device_disappearance_error(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;

    matches!(
        error.kind(),
        ErrorKind::NotFound
            | ErrorKind::BrokenPipe
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset
            | ErrorKind::UnexpectedEof
    ) || matches!(
        error.raw_os_error(),
        // Windows: FILE/PATH_NOT_FOUND, INVALID_HANDLE, NOT_READY,
        // DEVICE_NOT_CONNECTED. Unix: ENODEV.
        Some(2 | 3 | 6 | 21 | 1167 | 19)
    )
}

fn validate_uf2(bytes: &[u8], expected_family: u32) -> Result<()> {
    if bytes.is_empty() || bytes.len() % UF2_BLOCK_SIZE != 0 {
        return Err(FbuildError::DeployFailed(format!(
            "malformed RP2040 UF2: size {} is not a non-zero multiple of {UF2_BLOCK_SIZE}",
            bytes.len()
        )));
    }
    let block_count = bytes.len() / UF2_BLOCK_SIZE;
    let mut seen = vec![false; block_count];
    let mut seen_targets = BTreeSet::new();
    let flash_end = if expected_family == RP2350_FAMILY_ID {
        0x1400_0000u32
    } else {
        0x1100_0000u32
    };
    for (index, block) in bytes.chunks_exact(UF2_BLOCK_SIZE).enumerate() {
        let field = |offset: usize| {
            u32::from_le_bytes(
                block[offset..offset + 4]
                    .try_into()
                    .expect("four-byte UF2 field"),
            )
        };
        if field(0) != UF2_MAGIC_START0
            || field(4) != UF2_MAGIC_START1
            || field(508) != UF2_MAGIC_END
        {
            return Err(FbuildError::DeployFailed(format!(
                "malformed RP2040 UF2: invalid magic in block {index}"
            )));
        }
        if field(8) & UF2_FLAG_FAMILY_ID_PRESENT == 0 || field(28) != expected_family {
            return Err(FbuildError::DeployFailed(format!(
                "wrong UF2 family in block {index}: expected 0x{expected_family:08X}, found 0x{:08X}",
                field(28)
            )));
        }
        let target_address = field(12);
        let block_number = field(20) as usize;
        if field(16) != UF2_PAYLOAD_SIZE as u32
            || block_number >= block_count
            || field(24) as usize != block_count
            || target_address % UF2_PAYLOAD_SIZE as u32 != 0
            || !(RP2040_UF2_BASE_ADDRESS..flash_end).contains(&target_address)
        {
            return Err(FbuildError::DeployFailed(format!(
                "malformed RP2040 UF2 block metadata at block {index}"
            )));
        }
        if std::mem::replace(&mut seen[block_number], true) {
            return Err(FbuildError::DeployFailed(format!(
                "malformed RP2040 UF2: duplicate block number {block_number}"
            )));
        }
        if !seen_targets.insert(target_address) {
            return Err(FbuildError::DeployFailed(format!(
                "malformed RP2040 UF2: overlapping target address 0x{target_address:08X}"
            )));
        }
    }
    if seen.iter().any(|present| !present) {
        return Err(FbuildError::DeployFailed(
            "malformed RP2040 UF2: block-number sequence is incomplete".to_string(),
        ));
    }
    Ok(())
}

fn wait_for_volume_disappearance(volume: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !has_uf2_marker(volume) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(FbuildError::DeployFailed(format!(
        "RP2040 BOOTSEL volume {} did not eject after NEW.UF2; the ROM did not accept the image",
        volume.display()
    )))
}

/// Deploys RP2040-family firmware through the stock BOOTSEL mass-storage
/// interface. `bootloader_timeout` is configurable for deterministic tests.
pub struct Rp2040Deployer {
    bootloader_timeout: Duration,
    post_deploy_timeout: Duration,
    family_id: u32,
}

impl Default for Rp2040Deployer {
    fn default() -> Self {
        Self {
            bootloader_timeout: Duration::from_secs(10),
            // Windows can take several seconds to enumerate the CDC interface
            // after the ROM accepts the UF2. Keep this bounded but generous
            // enough for a stock board on a busy USB hub.
            post_deploy_timeout: Duration::from_secs(15),
            family_id: RP2040_FAMILY_ID,
        }
    }
}

impl Rp2040Deployer {
    pub fn new(bootloader_timeout: Duration, post_deploy_timeout: Duration) -> Self {
        Self {
            bootloader_timeout,
            post_deploy_timeout,
            family_id: RP2040_FAMILY_ID,
        }
    }

    pub fn for_mcu(mcu: &str) -> Result<Self> {
        let mut deployer = Self::default();
        match mcu.to_ascii_lowercase().as_str() {
            value if value.starts_with("rp2040") => {}
            value if value.starts_with("rp2350") => deployer.family_id = RP2350_FAMILY_ID,
            _ => {
                return Err(FbuildError::DeployFailed(format!(
                    "unsupported Raspberry Pi MCU {mcu:?}; expected rp2040 or rp2350"
                )));
            }
        }
        Ok(deployer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PicoCdcPort {
    name: String,
    serial_number: Option<String>,
}

fn catalogue_pico_cdc_ports(expected_family: u32) -> Result<Vec<PicoCdcPort>> {
    let ports = fbuild_serial::ports::available_ports().map_err(|error| {
        FbuildError::SerialError(format!(
            "failed to enumerate post-deploy serial ports: {error}"
        ))
    })?;
    let mut matches: Vec<PicoCdcPort> = ports
        .into_iter()
        .filter_map(|port| {
            let serialport::SerialPortType::UsbPort(usb) = &port.port_type else {
                return None;
            };
            let matches_family = fbuild_core::usb::profiles::profiles_for(usb.vid, usb.pid)
                .iter()
                .any(|profile| profile_matches_family(profile, expected_family));
            matches_family.then(|| PicoCdcPort {
                name: port.port_name,
                serial_number: usb.serial_number.clone(),
            })
        })
        .collect();
    matches.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(matches)
}

fn wait_for_cdc_port(
    previous_port: Option<&str>,
    requested_serial: Option<&str>,
    before: &BTreeSet<String>,
    expected_family: u32,
    timeout: Duration,
) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let ports = catalogue_pico_cdc_ports(expected_family)?;
        if let Some(selected) =
            select_cdc_candidate(previous_port, requested_serial, before, &ports)?
        {
            return Ok(selected);
        }
        if Instant::now() >= deadline {
            return Err(FbuildError::DeployFailed(
                "RP2040 firmware was transferred, but no catalogue-identified runtime CDC port appeared; verify that the firmware enables USB serial and that FastLED/boards USB data is current"
                    .to_string(),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn profile_matches_family(
    profile: &fbuild_core::usb::profiles::UsbTransportProfile,
    family_id: u32,
) -> bool {
    use fbuild_core::usb::profiles::{UsbDeviceRole, UsbPurpose};

    if profile.purpose != UsbPurpose::Runtime
        || profile.role != UsbDeviceRole::RuntimeCdc
        || profile.interface.as_deref() != Some("cdc")
    {
        return false;
    }
    let expected = if family_id == RP2350_FAMILY_ID {
        "rp2350"
    } else {
        "rp2040"
    };
    profile.family.as_deref() == Some(expected)
}

fn ensure_verified_usb_profiles() -> Result<()> {
    let dir = fbuild_paths::get_cache_root().join("usb");
    let meta_path = dir.join("_meta.json");
    let profiles_path = dir.join("usb-profiles.json");
    if fbuild_core::usb::profiles::populate_profiles_from_paths(&meta_path, &profiles_path) {
        Ok(())
    } else {
        Err(FbuildError::DeployFailed(
            "verified FastLED/boards USB profiles are unavailable; fbuild refuses to guess a Pico VID/PID or use a built-in fallback"
                .to_string(),
        ))
    }
}

#[async_trait::async_trait]
impl Deployer for Rp2040Deployer {
    async fn deploy(
        &self,
        project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult> {
        tokio::task::spawn_blocking(ensure_verified_usb_profiles)
            .await
            .map_err(|error| {
                FbuildError::DeployFailed(format!(
                    "failed to prepare the FastLED/boards USB profile cache: {error}"
                ))
            })??;
        let selector = port.map(str::trim).filter(|value| !value.is_empty());
        let family_id = self.family_id;
        let current_ports =
            tokio::task::spawn_blocking(move || catalogue_pico_cdc_ports(family_id))
                .await
                .map_err(|error| {
                    FbuildError::DeployFailed(format!(
                        "RP2040 serial snapshot task failed: {error}"
                    ))
                })??;
        let ports_before: BTreeSet<String> = current_ports
            .iter()
            .map(|port| port.name.clone())
            .collect();
        let explicit_volume = selector.and_then(explicit_uf2_volume);
        if selector.is_some_and(|value| value.to_ascii_lowercase().starts_with("uf2="))
            && explicit_volume.is_none()
        {
            return Err(FbuildError::DeployFailed(format!(
                "explicit RP2040 UF2 volume {selector:?} does not contain INFO_UF2.TXT"
            )));
        }
        let volume_before_reset = match explicit_volume {
            Some(volume) => Some(volume),
            None => select_single_uf2_volume(find_uf2_volumes(&volume_roots()))?,
        };
        let requested_serial = selector.and_then(serial_selector).map(str::to_string);
        let runtime_target = if volume_before_reset.is_none() {
            selector
                .map(|value| resolve_requested_runtime_target(value, &current_ports))
                .transpose()?
        } else {
            None
        };
        if let Some(target) = &runtime_target {
            let port = target.port.clone();
            tokio::task::spawn_blocking(move || touch_1200bps(&port))
                .await
                .map_err(|error| {
                    FbuildError::DeployFailed(format!("RP2040 reset task failed: {error}"))
                })??;
        }
        let volume = if let Some(volume) = volume_before_reset {
            volume
        } else {
            let timeout = self.bootloader_timeout;
            tokio::task::spawn_blocking(move || find_uf2_volume_until(timeout))
                .await
                .map_err(|error| {
                    FbuildError::DeployFailed(format!("RP2040 volume watcher failed: {error}"))
                })??
                .ok_or_else(|| {
                    FbuildError::DeployFailed(
                        "RP2040 BOOTSEL volume not found; check that the stock board is connected and retry"
                            .into(),
                    )
                })?
        };
        let firmware = firmware_path.to_path_buf();
        let family_id = self.family_id;
        let artifact = tokio::task::spawn_blocking(move || {
            prepare_uf2_artifact(&firmware, family_id)
        })
        .await
        .map_err(|error| {
            FbuildError::DeployFailed(format!("RP2040 UF2 preparation task failed: {error}"))
        })??;
        let artifact_for_copy = artifact.clone();
        let volume_for_copy = volume.clone();
        let copy_result = tokio::task::spawn_blocking(move || {
            copy_prepared_uf2(&artifact_for_copy, &volume_for_copy)
        })
        .await
        .map_err(|error| {
            FbuildError::DeployFailed(format!("RP2040 UF2 writer task failed: {error}"))
        })?;
        let (transfer_stdout, transfer_stderr, transfer_method) = match copy_result {
            Ok(destination) => (
                format!("wrote {}", destination.display()),
                String::new(),
                "BOOTSEL mass-storage",
            ),
            Err(copy_error) => {
                let loaded = picotool::load_with_managed_picotool(
                    project_dir,
                    &artifact,
                    &copy_error.to_string(),
                )
                .await?;
                (
                    loaded.stdout,
                    loaded.stderr,
                    "managed picotool fallback",
                )
            }
        };
        let volume_for_wait = volume.clone();
        let post_timeout = self.post_deploy_timeout;
        tokio::task::spawn_blocking(move || {
            wait_for_volume_disappearance(&volume_for_wait, post_timeout)
        })
        .await
        .map_err(|error| {
            FbuildError::DeployFailed(format!("RP2040 eject watcher failed: {error}"))
        })??;
        let recovery_port = runtime_target.as_ref().map(|target| target.port.clone());
        let recovery_serial = runtime_target
            .as_ref()
            .and_then(|target| target.serial_number.clone())
            .or(requested_serial);
        let family_id = self.family_id;
        let discovered_port = tokio::task::spawn_blocking(move || {
            wait_for_cdc_port(
                recovery_port.as_deref(),
                recovery_serial.as_deref(),
                &ports_before,
                family_id,
                post_timeout,
            )
        })
        .await
        .map_err(|error| {
            FbuildError::DeployFailed(format!("RP2040 CDC watcher failed: {error}"))
        })??;
        Ok(DeploymentResult {
            success: true,
            message: format!(
                "firmware deployed to RP2040 via {transfer_method} ({})",
                volume.display(),
            ),
            port: Some(discovered_port),
            stdout: transfer_stdout,
            stderr: transfer_stderr,
            outcome: DeployOutcome::FullFlash,
        })
    }

    async fn post_deploy_recovery(&self, port: &str) -> Result<()> {
        let deadline = Instant::now() + self.post_deploy_timeout;
        while Instant::now() < deadline {
            let port_name = port.to_string();
            let present = tokio::task::spawn_blocking(move || {
                serialport::new(&port_name, 115_200)
                    .timeout(Duration::from_millis(100))
                    .open()
                    .is_ok()
            })
            .await
            .unwrap_or(false);
            if present {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tracing::warn!("RP2040 CDC port {port} did not reappear after deploy");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn uf2_has_rp2040_family_and_expected_blocks() {
        let image = vec![0xA5; 300];
        let uf2 = encode_uf2(&image);
        assert_eq!(uf2.len(), UF2_BLOCK_SIZE * 2);
        assert_eq!(
            u32::from_le_bytes(uf2[0..4].try_into().unwrap()),
            UF2_MAGIC_START0
        );
        assert_eq!(
            u32::from_le_bytes(uf2[28..32].try_into().unwrap()),
            RP2040_FAMILY_ID
        );
        assert_eq!(
            u32::from_le_bytes(uf2[12..16].try_into().unwrap()),
            RP2040_UF2_BASE_ADDRESS
        );
        assert_eq!(
            u32::from_le_bytes(uf2[512 + 12..512 + 16].try_into().unwrap()),
            RP2040_UF2_BASE_ADDRESS + UF2_PAYLOAD_SIZE as u32
        );
        assert_eq!(
            u32::from_le_bytes(uf2[512 + 20..512 + 24].try_into().unwrap()),
            1
        );
        assert_eq!(
            u32::from_le_bytes(uf2[512 + 16..512 + 20].try_into().unwrap()),
            UF2_PAYLOAD_SIZE as u32
        );
        assert_eq!(&uf2[32..32 + 256], &image[..256]);
        assert_eq!(&uf2[512 + 32..512 + 32 + 44], &image[256..]);
        assert_eq!(
            u32::from_le_bytes(uf2[508..512].try_into().unwrap()),
            UF2_MAGIC_END
        );
    }

    #[test]
    fn uf2_magic_matches_the_published_byte_sequence() {
        assert_eq!(UF2_MAGIC_START0.to_le_bytes(), [0x55, 0x46, 0x32, 0x0A]);
        assert_eq!(UF2_MAGIC_START1.to_le_bytes(), [0x57, 0x51, 0x5D, 0x9E]);
        assert_eq!(UF2_MAGIC_END.to_le_bytes(), [0x30, 0x6F, 0xB1, 0x0A]);
    }

    #[test]
    fn full_image_boot2_uses_xip_address() {
        // Prefix emitted by the RP2040 second-stage bootloader in FastLED's
        // complete firmware.bin artifact.
        let boot2_prefix = [0x00, 0xB5, 0x32, 0x4B, 0x21, 0x20, 0x58, 0x60];
        let uf2 = encode_uf2(&boot2_prefix);
        assert_eq!(
            u32::from_le_bytes(uf2[12..16].try_into().unwrap()),
            0x1000_0000
        );
        assert_eq!(&uf2[32..40], &boot2_prefix);
    }

    #[test]
    fn finds_marker_volume_without_requiring_a_drive_letter() {
        let root = tempdir().unwrap();
        let volume = root.path().join("RPI-RP2");
        fs::create_dir(&volume).unwrap();
        fs::write(volume.join("INFO_UF2.TXT"), "Model: Raspberry Pi RP2").unwrap();
        assert_eq!(find_uf2_volume(&[root.path().to_path_buf()]), Some(volume));
    }

    #[test]
    fn overlapping_linux_roots_do_not_duplicate_one_volume() {
        let root = tempdir().unwrap();
        let user_root = root.path().join("media").join("alice");
        let volume = user_root.join("RPI-RP2");
        fs::create_dir_all(&volume).unwrap();
        fs::write(volume.join("INFO_UF2.TXT"), "Model: Raspberry Pi RP2").unwrap();
        assert_eq!(
            find_uf2_volumes(&[user_root, root.path().join("media")]),
            vec![volume]
        );
    }

    #[test]
    fn writes_bin_as_uf2_to_marker_volume() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("INFO_UF2.TXT"), "UF2 Bootloader").unwrap();
        let firmware = root.path().join("firmware.bin");
        fs::write(&firmware, [1u8, 2, 3]).unwrap();
        let destination = write_uf2(&firmware, root.path(), RP2040_FAMILY_ID).unwrap();
        assert_eq!(destination.file_name().unwrap(), "NEW.UF2");
        let artifact = firmware.with_extension("uf2");
        assert!(artifact.is_file());
        assert_eq!(
            fs::read(&artifact).unwrap(),
            fs::read(&destination).unwrap()
        );
        assert_eq!(
            fs::metadata(destination).unwrap().len(),
            UF2_BLOCK_SIZE as u64
        );
    }

    #[test]
    fn rejects_wrong_family_and_malformed_uf2() {
        let wrong = encode_uf2_for_family(&[1, 2, 3], RP2350_FAMILY_ID);
        let error = validate_uf2(&wrong, RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("wrong UF2 family"));

        let error = validate_uf2(&[0; 511], RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("malformed RP2040 UF2"));

        let mut duplicate = encode_uf2(&[0; 300]);
        duplicate[512 + 20..512 + 24].copy_from_slice(&0u32.to_le_bytes());
        let error = validate_uf2(&duplicate, RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("duplicate block number"));

        let mut bad_address = encode_uf2(&[0; 1]);
        bad_address[12..16].copy_from_slice(&0x2000_0000u32.to_le_bytes());
        let error = validate_uf2(&bad_address, RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("block metadata"));

        let mut overlapping = encode_uf2(&[0; 300]);
        overlapping[512 + 12..512 + 16]
            .copy_from_slice(&RP2040_UF2_BASE_ADDRESS.to_le_bytes());
        let error = validate_uf2(&overlapping, RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("overlapping target address"));
    }

    #[test]
    fn refuses_to_encode_elf_or_unknown_input_as_raw_flash() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("INFO_UF2.TXT"), "UF2 Bootloader").unwrap();
        for filename in ["firmware.elf", "firmware.dat"] {
            let firmware = root.path().join(filename);
            fs::write(&firmware, [0x7f, b'E', b'L', b'F']).unwrap();
            let error = write_uf2(&firmware, root.path(), RP2040_FAMILY_ID).unwrap_err();
            assert!(error.to_string().contains("expected a managed .uf2 or raw .bin"));
        }
    }

    #[test]
    fn already_mounted_explicit_volume_wins_without_serial_selection() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("INFO_UF2.TXT"), "Board-ID: RPI-RP2").unwrap();
        let selector = format!("UF2={}", root.path().display());
        assert_eq!(explicit_uf2_volume(&selector), Some(root.path().to_path_buf()));
        assert!(resolve_requested_runtime_target(&selector, &[]).is_err());
    }

    #[test]
    fn deploy_family_comes_from_mcu_not_board_name() {
        assert_eq!(
            Rp2040Deployer::for_mcu("rp2040").unwrap().family_id,
            RP2040_FAMILY_ID
        );
        assert_eq!(
            Rp2040Deployer::for_mcu("rp2350").unwrap().family_id,
            RP2350_FAMILY_ID
        );
    }

    #[test]
    fn typed_profile_must_match_runtime_role_interface_and_generation() {
        use fbuild_core::usb::profiles::{
            UsbDeviceRole, UsbIdentityMatch, UsbProfileProvenance, UsbPurpose,
            UsbTransportProfile,
        };

        let mut profile = UsbTransportProfile {
            identity_match: UsbIdentityMatch {
                vid: "feed".to_string(),
                pid: Some("c0de".to_string()),
                pid_mask: None,
            },
            purpose: UsbPurpose::Runtime,
            role: UsbDeviceRole::RuntimeCdc,
            transport: "usb".to_string(),
            reset: "touch-1200".to_string(),
            handoff: "bootloader".to_string(),
            platform: Some("synthetic".to_string()),
            family: Some("rp2040".to_string()),
            generation: Some("synthetic".to_string()),
            interface: Some("cdc".to_string()),
            provenance: UsbProfileProvenance {
                source_url: "test://fixture".to_string(),
                source_revision: "a".repeat(40),
                source_class: "test".to_string(),
            },
            priority: 100,
            allow_ambiguous: false,
        };
        assert!(profile_matches_family(&profile, RP2040_FAMILY_ID));
        assert!(!profile_matches_family(&profile, RP2350_FAMILY_ID));
        profile.family = Some("rp2350".to_string());
        assert!(profile_matches_family(&profile, RP2350_FAMILY_ID));
        profile.role = UsbDeviceRole::BootloaderUf2;
        assert!(!profile_matches_family(&profile, RP2350_FAMILY_ID));
    }

    #[test]
    fn successful_rom_transfer_waits_for_marker_disappearance() {
        let volume = tempdir().unwrap();
        let marker = volume.path().join("INFO_UF2.TXT");
        fs::write(&marker, "Model: Raspberry Pi RP2").unwrap();
        let marker_for_thread = marker.clone();
        let remover = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            fs::remove_file(marker_for_thread).unwrap();
        });
        wait_for_volume_disappearance(volume.path(), Duration::from_secs(1)).unwrap();
        remover.join().unwrap();
    }

    #[test]
    fn copy_error_is_accepted_only_when_rom_volume_disappears() {
        let root = tempdir().unwrap();
        let marker = root.path().join("INFO_UF2.TXT");
        let artifact = root.path().join("firmware.uf2");
        let destination = root.path().join("NEW.UF2");
        fs::write(&marker, "Model: Raspberry Pi RP2").unwrap();
        fs::write(&artifact, encode_uf2(&[1, 2, 3])).unwrap();

        copy_uf2_artifact_with(&artifact, &destination, root.path(), |_, _| {
            fs::remove_file(&marker).unwrap();
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "device disappeared after final block",
            ))
        })
        .unwrap();

        fs::write(&marker, "Model: Raspberry Pi RP2").unwrap();
        let error = copy_uf2_artifact_with(&artifact, &destination, root.path(), |_, _| {
            Err(std::io::Error::other("corrupt volume"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("corrupt volume"));

        fs::remove_file(&marker).unwrap();
        let error = copy_uf2_artifact_with(&artifact, &destination, root.path(), |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "source permission denied",
            ))
        })
        .unwrap_err();
        assert!(error.to_string().contains("source permission denied"));
    }
}
