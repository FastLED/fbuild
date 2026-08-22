//! Best-effort mounting for a stock RP-series ROM volume.
//!
//! Candidate discovery (`/dev/disk/by-id/usb-RPI_RP2*-part1`) is pure and
//! runs on every host; the actual mount mechanic lives behind
//! [`fbuild_core::platform::device::mount_block_devices`] (udisksctl on
//! Linux, a no-op where the OS auto-mounts). Callers keep the retry/policy
//! loop around this.

use std::path::{Path, PathBuf};

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

/// Try to mount any RP-series ROM block device the host exposes. Returns
/// whether any candidate existed — `false` means "nothing to mount", not
/// "mounting failed"; individual failures are logged, never fatal.
pub(super) fn try_mount_rom_device() -> bool {
    let devices = rom_block_devices(Path::new("/dev/disk/by-id"));
    let device_strs: Vec<String> = devices
        .iter()
        .map(|device| device.to_string_lossy().into_owned())
        .collect();
    let device_refs: Vec<&str> = device_strs.iter().map(String::as_str).collect();
    fbuild_core::platform::device::mount_block_devices(&device_refs);
    !devices.is_empty()
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
