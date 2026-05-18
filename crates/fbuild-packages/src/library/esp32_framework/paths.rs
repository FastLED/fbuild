//! Simple path accessors for ESP32 framework directories.

use std::path::PathBuf;

use super::fs_utils::collect_sources;
use super::sdk_paths::sdk_mcu_dir;
use super::Esp32Framework;

impl Esp32Framework {
    /// Get the core source directory (e.g. `cores/esp32`).
    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        self.resolved_dir().join("cores").join(core_name)
    }

    /// Get the variant directory for a board (e.g. `variants/esp32c6`).
    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.resolved_dir().join("variants").join(variant_name)
    }

    /// Get the linker scripts directory for a given MCU.
    pub fn get_linker_scripts_dir(&self, mcu: &str) -> PathBuf {
        sdk_mcu_dir(self, mcu).join("ld")
    }

    /// Get the path to the bootloader binary.
    ///
    /// First checks for a pre-built `bootloader.bin`. If not found, looks for
    /// `bootloader_{flash_mode}_{flash_freq}.elf` which needs elf2image conversion.
    pub fn get_bootloader_bin(&self, mcu: &str) -> PathBuf {
        sdk_mcu_dir(self, mcu).join("bin").join("bootloader.bin")
    }

    /// Get the path to the bootloader ELF for a given flash mode and frequency.
    ///
    /// ESP32 Arduino core provides pre-built bootloader ELFs named
    /// `bootloader_{mode}_{freq}.elf`. The ROM bootloader on ESP32-S3 and
    /// similar chips can only load the second-stage bootloader in DIO mode,
    /// so `flash_mode` should typically be "dio" regardless of application mode.
    pub fn get_bootloader_elf(&self, mcu: &str, flash_mode: &str, flash_freq: &str) -> PathBuf {
        let filename = format!("bootloader_{}_{}.elf", flash_mode, flash_freq);
        sdk_mcu_dir(self, mcu).join("bin").join(filename)
    }

    /// Get the path to the partitions binary.
    pub fn get_partitions_bin(&self, mcu: &str) -> PathBuf {
        sdk_mcu_dir(self, mcu).join("bin").join("partitions.bin")
    }

    /// Get the path to the default ESP-IDF boot_app0 helper binary.
    pub fn get_boot_app0_bin(&self) -> PathBuf {
        self.resolved_dir()
            .join("tools")
            .join("partitions")
            .join("boot_app0.bin")
    }

    /// Get the path to the partitions CSV file.
    pub fn get_partitions_csv(&self, partitions_name: &str) -> PathBuf {
        self.resolved_dir()
            .join("tools")
            .join("partitions")
            .join(partitions_name)
    }

    /// Get the path to gen_esp32part.py for generating partition tables.
    pub fn get_gen_esp32part(&self) -> PathBuf {
        self.resolved_dir().join("tools").join("gen_esp32part.py")
    }

    /// List all source files in a core directory.
    pub fn get_core_sources(&self, core_name: &str) -> Vec<PathBuf> {
        collect_sources(&self.get_core_dir(core_name))
    }
}
