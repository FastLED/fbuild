//! Linux best-effort mounting for a stock RP-series ROM volume.

#[cfg(any(target_os = "linux", test))]
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "linux", test))]
fn rom_block_devices(by_id: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(by_id) else {
        return Vec::new();
    };
    let mut devices: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            (name.starts_with("usb-RPI_RP2") && name.ends_with("-part1")).then(|| entry.path())
        })
        .collect();
    devices.sort();
    devices
}

#[cfg(target_os = "linux")]
pub(super) fn try_mount_linux_rom_device() -> bool {
    let devices = rom_block_devices(Path::new("/dev/disk/by-id"));
    for device in &devices {
        let device = device.to_string_lossy().to_string();
        let args = ["udisksctl", "mount", "--block-device", device.as_str()];
        match fbuild_core::subprocess::run_command_blocking(
            &args,
            None,
            None,
            Some(std::time::Duration::from_secs(5)),
        ) {
            Ok(output) if output.success() => {
                tracing::debug!(device, "mounted RP-series ROM volume with udisksctl");
            }
            Ok(output) => {
                tracing::debug!(
                    device,
                    stderr = output.stderr.trim(),
                    "udisksctl could not mount RP-series ROM volume"
                );
            }
            Err(error) => {
                tracing::debug!(device, error = %error, "RP-series ROM auto-mount unavailable");
            }
        }
    }
    !devices.is_empty()
}

#[cfg(not(target_os = "linux"))]
pub(super) fn try_mount_linux_rom_device() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_only_rpi_rp2_partition_links() {
        let temp = tempfile::tempdir().unwrap();
        for name in [
            "usb-RPI_RP2_E0C91234-if01-part1",
            "usb-RPI_RP2_E0C91234-if01",
            "usb-OTHER_DEVICE-part1",
        ] {
            std::fs::write(temp.path().join(name), []).unwrap();
        }
        assert_eq!(
            rom_block_devices(temp.path()),
            vec![temp.path().join("usb-RPI_RP2_E0C91234-if01-part1")]
        );
    }
}
