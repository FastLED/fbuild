//! RP2040/RP2350 deployment through the stock UF2 BOOTSEL volume.
//!
//! A Raspberry Pi Pico does not require a vendor flashing utility: opening
//! its CDC port at 1200 baud and closing it enters BOOTSEL, where the ROM
//! exposes a mass-storage volume containing `INFO_UF2.TXT`. Copying a UF2
//! file to that volume is the documented stock-board deployment path.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fbuild_core::{FbuildError, Result};

use crate::{DeployOutcome, Deployer, DeploymentResult};

const UF2_MAGIC_START0: u32 = 0x0A32_4555;
const UF2_MAGIC_START1: u32 = 0x9E5D_5157;
const UF2_MAGIC_END: u32 = 0x0AB1_6F30;
const UF2_FLAG_FAMILY_ID_PRESENT: u32 = 0x0000_2000;
const RP2040_FAMILY_ID: u32 = 0xE48B_FF56;
const RP2350_FAMILY_ID: u32 = 0xE48B_FF59;
const UF2_PAYLOAD_SIZE: usize = 256;
const UF2_BLOCK_SIZE: usize = 512;

/// Build UF2 blocks for a raw RP2040 flash image.
pub fn encode_uf2(binary: &[u8]) -> Vec<u8> {
    encode_uf2_for_family(binary, RP2040_FAMILY_ID)
}

/// Build UF2 blocks using an explicit Raspberry Pi family identifier.
pub fn encode_uf2_for_family(binary: &[u8], family_id: u32) -> Vec<u8> {
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
        put_u32(&mut block, 12, start as u32 + 0x1000_0000);
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
    let mut matches = Vec::new();
    for root in roots {
        if has_uf2_marker(root) {
            matches.push(root.clone());
        }
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && has_uf2_marker(&path) {
                matches.push(path);
            }
        }
    }
    matches
}

fn find_uf2_volume_until(timeout: Duration) -> Result<Option<PathBuf>> {
    let deadline = Instant::now() + timeout;
    loop {
        let matches = find_uf2_volumes(&volume_roots());
        if matches.len() > 1 {
            return Err(FbuildError::DeployFailed(format!(
                "found multiple RP2040 BOOTSEL volumes: {}",
                matches
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if let Some(path) = matches.into_iter().next() {
            return Ok(Some(path));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn touch_1200bps(port: &str) -> Result<()> {
    match serialport::new(port, 1200)
        .timeout(Duration::from_secs(2))
        .open()
    {
        Ok(_) => {
            // The Pico ROM observes the baud rate on close. The short hold
            // avoids racing CDC ACM implementations that apply it lazily.
            std::thread::sleep(Duration::from_millis(100));
            Ok(())
        }
        Err(error) if error.kind() == serialport::ErrorKind::NoDevice => Ok(()),
        Err(error) => Err(FbuildError::SerialError(format!(
            "failed to enter RP2040 BOOTSEL on {port}: {error}"
        ))),
    }
}

fn write_uf2(firmware_path: &Path, volume: &Path, family_id: u32) -> Result<PathBuf> {
    let bytes = fs::read(firmware_path).map_err(|error| {
        FbuildError::DeployFailed(format!(
            "failed to read {}: {error}",
            firmware_path.display()
        ))
    })?;
    let uf2 = if firmware_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("uf2"))
    {
        bytes
    } else {
        encode_uf2_for_family(&bytes, family_id)
    };
    let destination = volume.join("firmware.uf2");
    let mut file = fs::File::create(&destination).map_err(|error| {
        FbuildError::DeployFailed(format!(
            "failed to create UF2 at {}: {error}",
            destination.display()
        ))
    })?;
    std::io::Write::write_all(&mut file, &uf2).map_err(|error| {
        FbuildError::DeployFailed(format!(
            "failed to copy UF2 to {}: {error}",
            destination.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        FbuildError::DeployFailed(format!(
            "failed to flush UF2 to {}: {error}",
            destination.display()
        ))
    })?;
    Ok(destination)
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

    pub fn for_board(board_id: &str) -> Self {
        let mut deployer = Self::default();
        if board_id.to_ascii_lowercase().contains("pico2") {
            deployer.family_id = RP2350_FAMILY_ID;
        }
        deployer
    }
}

fn wait_for_cdc_port(previous: Option<&str>, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(ports) = fbuild_serial::ports::available_ports() {
            let names: Vec<String> = ports
                .into_iter()
                .filter(|port| {
                    matches!(
                        port.port_type,
                        serialport::SerialPortType::UsbPort(ref usb)
                            if usb.vid == 0x2E8A
                    )
                })
                .map(|port| port.port_name)
                .collect();
            if let Some(old) = previous {
                if names.iter().any(|name| name == old) {
                    return Some(old.to_string());
                }
            } else if let Some(name) = names.into_iter().next() {
                return Some(name);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[async_trait::async_trait]
impl Deployer for Rp2040Deployer {
    async fn deploy(
        &self,
        _project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult> {
        let port = port.map(str::trim).filter(|value| !value.is_empty());
        let original_port = port.map(str::to_string);
        if let Some(port) = port {
            let port = port.to_string();
            tokio::task::spawn_blocking(move || touch_1200bps(&port))
                .await
                .map_err(|error| {
                    FbuildError::DeployFailed(format!("RP2040 reset task failed: {error}"))
                })??;
        }
        let timeout = self.bootloader_timeout;
        let volume = tokio::task::spawn_blocking(move || find_uf2_volume_until(timeout))
            .await
            .map_err(|error| FbuildError::DeployFailed(format!("RP2040 volume watcher failed: {error}")))?
            ?
            .ok_or_else(|| FbuildError::DeployFailed(
                "RP2040 BOOTSEL volume not found; check that the stock board is connected and retry".into(),
            ))?;
        let firmware = firmware_path.to_path_buf();
        let volume_for_copy = volume.clone();
        let family_id = self.family_id;
        let destination =
            tokio::task::spawn_blocking(move || write_uf2(&firmware, &volume_for_copy, family_id))
                .await
                .map_err(|error| {
                    FbuildError::DeployFailed(format!("RP2040 UF2 writer failed: {error}"))
                })??;
        let recovery_port = original_port.clone();
        let post_timeout = self.post_deploy_timeout;
        let discovered_port = tokio::task::spawn_blocking(move || {
            wait_for_cdc_port(recovery_port.as_deref(), post_timeout)
        })
        .await
        .unwrap_or(None)
        .or(original_port);
        Ok(DeploymentResult {
            success: true,
            message: format!(
                "firmware copied to RP2040 BOOTSEL volume {}",
                volume.display()
            ),
            port: discovered_port,
            stdout: format!("wrote {}", destination.display()),
            stderr: String::new(),
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
    fn finds_marker_volume_without_requiring_a_drive_letter() {
        let root = tempdir().unwrap();
        let volume = root.path().join("RPI-RP2");
        fs::create_dir(&volume).unwrap();
        fs::write(volume.join("INFO_UF2.TXT"), "Model: Raspberry Pi RP2").unwrap();
        assert_eq!(find_uf2_volume(&[root.path().to_path_buf()]), Some(volume));
    }

    #[test]
    fn writes_bin_as_uf2_to_marker_volume() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("INFO_UF2.TXT"), "UF2 Bootloader").unwrap();
        let firmware = root.path().join("firmware.bin");
        fs::write(&firmware, [1u8, 2, 3]).unwrap();
        let destination = write_uf2(&firmware, root.path(), RP2040_FAMILY_ID).unwrap();
        assert_eq!(destination.file_name().unwrap(), "firmware.uf2");
        assert_eq!(
            fs::metadata(destination).unwrap().len(),
            UF2_BLOCK_SIZE as u64
        );
    }
}
