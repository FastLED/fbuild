//! Prepare bootloader.bin, partitions.bin and boot_app0.bin for deployment / emulation.

use std::path::Path;
use std::time::Instant;

use fbuild_core::Result;

use super::super::mcu_config::Esp32McuConfig;

/// Stage boot/partition/boot_app0 binaries into `build_dir`. Logs warnings on
/// missing inputs but does not error — the linker output remains usable for
/// in-tree flows even when the boot artifacts cannot be produced. The one
/// exception: an explicitly configured `board_build.partitions` CSV that
/// resolves nowhere is a hard error (FastLED/fbuild#955) — silently flashing
/// a different partition table is worse than failing.
#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_boot_artifacts(
    build_dir: &Path,
    project_dir: &Path,
    framework: &fbuild_packages::library::Esp32Framework,
    board: &fbuild_config::BoardConfig,
    mcu_config: &Esp32McuConfig,
    flash_freq: &str,
    esptool_bin: Option<&Path>,
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
            // Prefer the provisioned standalone esptool binary; fall back to an
            // `esptool` on PATH (FastLED/fbuild#954).
            let argv = crate::esp32::esp32_linker::esptool_elf2image_argv(
                esptool_bin,
                &board.mcu,
                boot_flash_mode,
                flash_freq,
                flash_size,
                &boot_elf_str,
                &boot_dst_str,
            );
            let args: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
            match fbuild_core::subprocess::run_command(
                &args,
                None,
                None,
                Some(std::time::Duration::from_secs(60)),
            )
            .await
            {
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
        let parts_csv = resolve_partitions_csv(
            project_dir,
            framework.get_partitions_csv(partitions_name),
            board.partitions.as_deref(),
        )?;
        let gen_tool = framework.get_gen_esp32part();
        if parts_csv.exists() && gen_tool.exists() {
            let gen_tool_str = gen_tool.to_string_lossy();
            let parts_csv_str = parts_csv.to_string_lossy();
            let parts_dst_str = parts_dst.to_string_lossy();
            // `python` doesn't exist on modern distros (ubuntu 24.04 ships
            // only `python3`); resolve the interpreter the same way the
            // extra_scripts runtime does.
            let python = crate::script_runtime::find_python()
                .await
                .unwrap_or_else(|| vec!["python".to_string()]);
            let mut args: Vec<&str> = python.iter().map(|s| s.as_str()).collect();
            args.extend([
                gen_tool_str.as_ref(),
                "-q",
                parts_csv_str.as_ref(),
                parts_dst_str.as_ref(),
            ]);
            match fbuild_core::subprocess::run_command(
                &args,
                None,
                None,
                Some(std::time::Duration::from_secs(10)),
            )
            .await
            {
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

/// Resolve the partitions CSV with PlatformIO semantics
/// (FastLED/fbuild#955): an explicitly configured `board_build.partitions`
/// path is project-relative first; the framework `tools/partitions/`
/// directory serves built-in names (`default.csv`, `huge_app.csv`, ...).
/// An explicit CSV that resolves nowhere is a hard error. When nothing was
/// configured, the framework default is returned as-is and the caller's
/// existence check keeps the historical warn-and-continue behavior.
fn resolve_partitions_csv(
    project_dir: &Path,
    framework_candidate: std::path::PathBuf,
    configured: Option<&str>,
) -> Result<std::path::PathBuf> {
    let Some(name) = configured else {
        return Ok(framework_candidate);
    };
    let project_candidate = project_dir.join(name);
    if project_candidate.exists() {
        return Ok(project_candidate);
    }
    if framework_candidate.exists() {
        return Ok(framework_candidate);
    }
    Err(fbuild_core::FbuildError::BuildFailed(format!(
        "board_build.partitions = {} not found: checked {} and {}",
        name,
        project_candidate.display(),
        framework_candidate.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_partitions_csv_resolves_project_relative_first() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path();
        std::fs::create_dir_all(project.join("config")).unwrap();
        std::fs::write(project.join("config/custom.csv"), "csv").unwrap();

        let resolved = resolve_partitions_csv(
            project,
            project.join("framework/tools/partitions/config/custom.csv"),
            Some("config/custom.csv"),
        )
        .unwrap();
        assert_eq!(resolved, project.join("config/custom.csv"));
    }

    #[test]
    fn explicit_builtin_name_falls_back_to_framework_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path();
        let fw_dir = project.join("framework/tools/partitions");
        std::fs::create_dir_all(&fw_dir).unwrap();
        std::fs::write(fw_dir.join("huge_app.csv"), "csv").unwrap();

        let resolved =
            resolve_partitions_csv(project, fw_dir.join("huge_app.csv"), Some("huge_app.csv"))
                .unwrap();
        assert_eq!(resolved, fw_dir.join("huge_app.csv"));
    }

    #[test]
    fn explicit_partitions_csv_missing_everywhere_is_a_hard_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path();

        let err = resolve_partitions_csv(
            project,
            project.join("framework/tools/partitions/nope.csv"),
            Some("nope.csv"),
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("board_build.partitions = nope.csv"));
    }

    #[test]
    fn unconfigured_default_keeps_warn_and_continue_semantics() {
        let tmp = tempfile::TempDir::new().unwrap();
        let missing_default = tmp.path().join("framework/tools/partitions/default.csv");
        let resolved = resolve_partitions_csv(tmp.path(), missing_default.clone(), None).unwrap();
        assert_eq!(resolved, missing_default);
    }
}
