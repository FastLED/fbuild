//! Prepare bootloader.bin, partitions.bin and boot_app0.bin for deployment / emulation.

use std::path::Path;
use std::time::Instant;

use fbuild_core::Result;

use super::super::mcu_config::Esp32McuConfig;

/// Stage boot/partition/boot_app0 binaries into `build_dir`. Logs warnings on
/// missing inputs but does not error — the linker output remains usable for
/// in-tree flows even when the boot artifacts cannot be produced.
pub(super) fn prepare_boot_artifacts(
    build_dir: &Path,
    framework: &fbuild_packages::library::Esp32Framework,
    board: &fbuild_config::BoardConfig,
    mcu_config: &Esp32McuConfig,
    flash_freq: &str,
    perf: &mut crate::perf_log::PerfTimer,
) -> Result<()> {
    let boot_artifacts_started = Instant::now();
    perf.checkpoint("boot-artifacts-start");
    // SDK directory selector matching the chip's ROM revision (e.g. `esp32p4_es`
    // for ESP32-P4 eco0–eco2). The bootloader ELF must come from the same SDK
    // variant the app is linked against, or the ROM jumps into an illegal
    // instruction at the bootloader entry point.
    let sdk_variant = board.sdk_variant();
    let boot_dst = build_dir.join("bootloader.bin");
    let boot_bin_src = framework.get_bootloader_bin(sdk_variant);
    if boot_bin_src.exists() {
        // Pre-built bootloader.bin available — just copy
        std::fs::copy(&boot_bin_src, &boot_dst)?;
        tracing::info!("copied bootloader.bin");
    } else {
        // Convert bootloader ELF to BIN using esptool elf2image.
        //
        // CRITICAL: The ESP32 ROM bootloader can only fetch the second-stage
        // bootloader from flash in DIO mode (or OPI mode for octal-SPI
        // boards). Even if the application is QIO, the bootloader itself
        // must be DIO — otherwise the ROM bootloader cannot read it and
        // the chip enters a watchdog reset loop with `Saved PC` pointing
        // into ROM (e.g. `0x400454d5` on ESP32-S3).
        //
        // The Arduino-ESP32 framework ships pre-built ELFs named
        // `bootloader_<mode>_<freq>.elf` for this exact reason; we pick
        // `bootloader_dio_80m.elf` for non-OPI boards and pass
        // `--flash-mode dio` to esptool so the resulting BIN header
        // (byte 0x02) has the correct mode. The application's flash
        // mode (which may be QIO/QOUT/etc) is unaffected — that mode
        // is encoded in the firmware.bin and applied later by the
        // second-stage bootloader.
        //
        // We treat the app's `flash_mode` as OPI iff it equals "opi";
        // every other value (qio, qout, dio, dout, undefined) maps to
        // a DIO bootloader. Frequency is taken as-is because the bootloader
        // ELFs only exist at 80m (and 120m for QIO chips), and the
        // ROM bootloader runs at the boot frequency anyway.
        let app_flash_mode = board
            .flash_mode
            .as_deref()
            .unwrap_or(mcu_config.default_flash_mode());
        let boot_flash_mode = if app_flash_mode == "opi" {
            "opi"
        } else {
            "dio"
        };
        let boot_elf = framework.get_bootloader_elf(sdk_variant, boot_flash_mode, flash_freq);
        if boot_elf.exists() {
            let boot_elf_str = boot_elf.to_string_lossy();
            let boot_dst_str = boot_dst.to_string_lossy();
            let flash_size = crate::esp32::mcu_config::bytes_to_flash_size(
                board.max_flash,
                mcu_config.default_flash_size(),
            );
            let args = [
                "esptool",
                "--chip",
                &board.mcu,
                "elf2image",
                "--flash-mode",
                boot_flash_mode,
                "--flash-freq",
                flash_freq,
                "--flash-size",
                flash_size,
                &boot_elf_str,
                "-o",
                &boot_dst_str,
            ];
            match fbuild_core::subprocess::run_command(
                &args,
                None,
                None,
                Some(std::time::Duration::from_secs(30)),
            ) {
                Ok(result) if result.success() => {
                    tracing::info!("converted bootloader ELF → bootloader.bin");
                }
                Ok(result) => {
                    tracing::warn!(
                        "bootloader elf2image failed: {}{}",
                        result.stderr,
                        result.stdout
                    );
                }
                Err(e) => {
                    tracing::warn!("esptool not found for bootloader conversion: {}", e);
                }
            }
        } else {
            tracing::warn!(
                "no bootloader found at {} or {}",
                boot_bin_src.display(),
                boot_elf.display()
            );
        }
    }

    let parts_dst = build_dir.join("partitions.bin");
    let parts_bin_src = framework.get_partitions_bin(sdk_variant);
    if parts_bin_src.exists() {
        // Pre-built partitions.bin available — just copy
        std::fs::copy(&parts_bin_src, &parts_dst)?;
        tracing::info!("copied partitions.bin");
    } else {
        // Generate partitions.bin from CSV using gen_esp32part.py
        let partitions_name = board.partitions.as_deref().unwrap_or("default.csv");
        let parts_csv = framework.get_partitions_csv(partitions_name);
        let gen_tool = framework.get_gen_esp32part();
        if parts_csv.exists() && gen_tool.exists() {
            let gen_tool_str = gen_tool.to_string_lossy();
            let parts_csv_str = parts_csv.to_string_lossy();
            let parts_dst_str = parts_dst.to_string_lossy();
            let args = [
                "python",
                &gen_tool_str,
                "-q",
                &parts_csv_str,
                &parts_dst_str,
            ];
            match fbuild_core::subprocess::run_command(
                &args,
                None,
                None,
                Some(std::time::Duration::from_secs(10)),
            ) {
                Ok(result) if result.success() => {
                    tracing::info!("generated partitions.bin from {}", partitions_name);
                }
                Ok(result) => {
                    tracing::warn!("gen_esp32part.py failed: {}", result.stderr);
                }
                Err(e) => {
                    tracing::warn!("python not found for partitions generation: {}", e);
                }
            }
        } else {
            tracing::warn!(
                "no partitions source: csv={} gen_tool={}",
                parts_csv.display(),
                gen_tool.display()
            );
        }
    }

    let boot_app0_src = framework.get_boot_app0_bin();
    let boot_app0_dst = build_dir.join("boot_app0.bin");
    if boot_app0_src.exists() {
        std::fs::copy(&boot_app0_src, &boot_app0_dst)?;
        tracing::info!("copied boot_app0.bin");
    }
    perf.record("boot-artifacts", boot_artifacts_started.elapsed());
    perf.checkpoint("boot-artifacts-finish");
    Ok(())
}
