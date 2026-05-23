//! QEMU flash image assembly and argv builders for ESP32-family emulation.

use std::path::{Path, PathBuf};

use fbuild_core::Result;

use super::image::{fill_with_ff, patch_qemu_esp32s3_adc_calibration, write_binary_at_offset};
use super::parse::{parse_flash_size_bytes, parse_hex_offset};

/// Resolve the flash-image size to use for QEMU.
///
/// ESP32 QEMU supports 2MB, 4MB, 8MB, and 16MB flash images. We derive
/// the size from the board's `maximum_size` when present, otherwise fall back
/// to the MCU config's default flash size label.
pub fn resolve_qemu_flash_size_bytes(
    board: &fbuild_config::BoardConfig,
    default_flash_size: &str,
) -> Result<u64> {
    let size_bytes = match board.max_flash {
        Some(bytes) => bytes,
        None => parse_flash_size_bytes(default_flash_size)?,
    };
    if matches!(size_bytes, 2_097_152 | 4_194_304 | 8_388_608 | 16_777_216) {
        Ok(size_bytes)
    } else {
        Err(fbuild_core::FbuildError::DeployFailed(format!(
            "ESP32 QEMU supports only 2MB, 4MB, 8MB, or 16MB flash images; got {} bytes",
            size_bytes
        )))
    }
}

/// Create a merged raw flash image for ESP32 QEMU from bootloader,
/// partitions, and application firmware.
///
/// When `elf_path` is `Some`, the ESP32-S3 ADC calibration patch is applied.
/// Pass `None` for non-S3 variants to skip the patch.
pub fn create_qemu_flash_image(
    firmware_path: &Path,
    output_path: &Path,
    flash_size_bytes: u64,
    bootloader_offset: &str,
    partitions_offset: &str,
    firmware_offset: &str,
    elf_path: Option<&Path>,
) -> Result<PathBuf> {
    let build_dir = firmware_path.parent().unwrap_or_else(|| Path::new("."));
    let bootloader_path = build_dir.join("bootloader.bin");
    let boot_app0_path = build_dir.join("boot_app0.bin");
    let partitions_path = build_dir.join("partitions.bin");
    let firmware_offset = parse_hex_offset(firmware_offset)?;

    for required in [&bootloader_path, &partitions_path, firmware_path] {
        if !required.is_file() {
            return Err(fbuild_core::FbuildError::DeployFailed(format!(
                "required QEMU artifact not found: {}",
                required.display()
            )));
        }
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut output = std::fs::File::create(output_path)?;
    fill_with_ff(&mut output, flash_size_bytes)?;

    write_binary_at_offset(
        &mut output,
        &bootloader_path,
        parse_hex_offset(bootloader_offset)?,
        flash_size_bytes,
    )?;
    write_binary_at_offset(
        &mut output,
        &partitions_path,
        parse_hex_offset(partitions_offset)?,
        flash_size_bytes,
    )?;
    if boot_app0_path.is_file() {
        write_binary_at_offset(&mut output, &boot_app0_path, 0xE000, flash_size_bytes)?;
    }
    write_binary_at_offset(
        &mut output,
        firmware_path,
        firmware_offset,
        flash_size_bytes,
    )?;
    if let Some(elf_path) = elf_path {
        patch_qemu_esp32s3_adc_calibration(output_path, firmware_path, elf_path, firmware_offset)?;
    }

    Ok(output_path.to_path_buf())
}

/// Build the QEMU argv for ESP32-family emulation.
///
/// The `mcu` parameter selects the QEMU machine type and watchdog timer
/// driver name. Supported values: `esp32`, `esp32s3` (via
/// `qemu-system-xtensa`) and `esp32c3`, `esp32c6`, `esp32h2` (via
/// `qemu-system-riscv32`). Callers are responsible for launching the
/// matching QEMU binary; this function only emits the argv.
pub fn build_qemu_args(
    mcu: &str,
    flash_image: &Path,
    psram: Option<fbuild_config::Esp32QemuPsramConfig>,
) -> Vec<String> {
    let machine = mcu.to_lowercase();
    let mut args = vec![
        "-nographic".to_string(),
        "-machine".to_string(),
        machine.clone(),
    ];
    if let Some(psram) = psram {
        args.push("-m".to_string());
        args.push(format!("{}M", psram.size_mib));
    }
    args.extend([
        "-drive".to_string(),
        format!("file={},if=mtd,format=raw", flash_image.display()),
        "-serial".to_string(),
        "mon:stdio".to_string(),
        "-monitor".to_string(),
        "none".to_string(),
        "-global".to_string(),
        format!(
            "driver=timer.{}.timg,property=wdt_disable,value=true",
            machine
        ),
    ]);
    if let Some(psram) = psram {
        if psram.is_octal {
            args.push("-global".to_string());
            args.push("driver=ssi_psram,property=is_octal,value=true".to_string());
        }
    }
    args
}

/// Build the QEMU argv for ESP32-S3 emulation (convenience wrapper).
pub fn build_qemu_esp32s3_args(
    flash_image: &Path,
    psram: Option<fbuild_config::Esp32QemuPsramConfig>,
) -> Vec<String> {
    build_qemu_args("esp32s3", flash_image, psram)
}
