//! ESP32 deployer using esptool.py.
//!
//! Flashes firmware to ESP32 boards via serial port using esptool.
//! Bootloader offset varies by MCU:
//! - `0x1000`: esp32, esp32s2
//! - `0x0`: esp32c2, esp32c3, esp32c5, esp32c6, esp32h2, esp32s3
//! - `0x2000`: esp32p4

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::Result;
use object::{Object, ObjectSymbol};
use sha2::{Digest, Sha256};

use crate::{Deployer, DeploymentResult};

/// Esptool flash parameters sourced from MCU config JSON.
///
/// All fields correspond to `esptool` section fields in the MCU config.
pub struct EsptoolParams {
    pub flash_mode: String,
    pub flash_freq: String,
    pub default_baud: String,
    pub before_reset: String,
    pub after_reset: String,
}

/// ESP32 deployer using `esptool`.
pub struct Esp32Deployer {
    /// MCU chip type for esptool --chip flag (e.g. "esp32c6").
    chip: String,
    /// Baud rate for flashing (e.g. "460800").
    baud_rate: String,
    /// Flash offsets.
    bootloader_offset: String,
    partitions_offset: String,
    firmware_offset: String,
    /// Flash mode for esptool (e.g. "dio", "qio").
    flash_mode: String,
    /// Flash frequency for esptool (e.g. "80m", "40m").
    flash_freq: String,
    /// Reset mode before flashing.
    before_reset: String,
    /// Reset mode after flashing.
    after_reset: String,
    verbose: bool,
}

impl Esp32Deployer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chip: &str,
        baud_rate: &str,
        bootloader_offset: &str,
        partitions_offset: &str,
        firmware_offset: &str,
        esptool_params: &EsptoolParams,
        verbose: bool,
    ) -> Self {
        Self {
            chip: chip.to_string(),
            baud_rate: baud_rate.to_string(),
            bootloader_offset: bootloader_offset.to_string(),
            partitions_offset: partitions_offset.to_string(),
            firmware_offset: firmware_offset.to_string(),
            flash_mode: esptool_params.flash_mode.clone(),
            flash_freq: esptool_params.flash_freq.clone(),
            before_reset: esptool_params.before_reset.clone(),
            after_reset: esptool_params.after_reset.clone(),
            verbose,
        }
    }

    /// Create an ESP32 deployer from board config with explicit flash offsets.
    pub fn from_board_config(
        board: &fbuild_config::BoardConfig,
        bootloader_offset: &str,
        partitions_offset: &str,
        firmware_offset: &str,
        esptool_params: &EsptoolParams,
        verbose: bool,
    ) -> Self {
        let baud = board
            .upload_speed
            .as_deref()
            .unwrap_or(&esptool_params.default_baud);
        // Board-level flash_mode overrides MCU default.
        let flash_mode = board
            .flash_mode
            .as_deref()
            .unwrap_or(&esptool_params.flash_mode);
        let params = EsptoolParams {
            flash_mode: flash_mode.to_string(),
            flash_freq: esptool_params.flash_freq.clone(),
            default_baud: esptool_params.default_baud.clone(),
            before_reset: esptool_params.before_reset.clone(),
            after_reset: esptool_params.after_reset.clone(),
        };
        Self::new(
            &board.mcu,
            baud,
            bootloader_offset,
            partitions_offset,
            firmware_offset,
            &params,
            verbose,
        )
    }

    /// Override the baud rate (e.g. from a CLI `--baud` flag).
    pub fn with_baud_rate(mut self, baud: &str) -> Self {
        self.baud_rate = baud.to_string();
        self
    }

    /// Find the esptool executable.
    ///
    /// Uses standalone `esptool` command (available when esptool is pip-installed).
    fn find_esptool() -> Vec<String> {
        vec!["esptool".to_string()]
    }

    /// Build the `esptool verify-flash` command line that this deployer
    /// would run to verify a candidate firmware image is already on the
    /// device. Pure (no I/O) so we can unit-test the argument layout
    /// without touching real hardware.
    ///
    /// `verify-flash` uses the device-side `FLASH_MD5SUM` command (issued
    /// by the stub flasher), so it does NOT read the entire flash region
    /// back over UART — verification of a 2.4 MB ESP32-S3 image takes
    /// ~6 seconds end-to-end vs ~25 seconds for a full re-flash.
    /// See ISSUES.md "Fast deploy via verify-then-skip".
    pub fn build_verify_flash_args(&self, firmware_path: &Path, port: &str) -> Vec<String> {
        let build_dir = firmware_path.parent().unwrap_or_else(|| Path::new("."));
        let bootloader_path = build_dir.join("bootloader.bin");
        let partitions_path = build_dir.join("partitions.bin");

        let mut args = Self::find_esptool();
        args.extend([
            "--chip".to_string(),
            self.chip.clone(),
            "--port".to_string(),
            port.to_string(),
            "--baud".to_string(),
            self.baud_rate.clone(),
            "--before".to_string(),
            self.before_reset.clone(),
            "--after".to_string(),
            self.after_reset.clone(),
            "verify-flash".to_string(),
        ]);

        // Verify all three regions in a single esptool invocation so we
        // pay the stub-flasher upload cost (~3s) once, not three times.
        if bootloader_path.exists() {
            args.push(self.bootloader_offset.clone());
            args.push(bootloader_path.to_string_lossy().to_string());
        }
        if partitions_path.exists() {
            args.push(self.partitions_offset.clone());
            args.push(partitions_path.to_string_lossy().to_string());
        }
        args.push(self.firmware_offset.clone());
        args.push(firmware_path.to_string_lossy().to_string());
        args
    }

    /// Run `esptool verify-flash` on bootloader + partitions + firmware
    /// against the live device. Returns `Ok(true)` when every region's
    /// FLASH_MD5SUM matches the local file (i.e. flashing would be a
    /// no-op), `Ok(false)` when at least one region differs, and `Err`
    /// only when esptool itself failed to run (port not found, stub
    /// upload failed, etc.).
    ///
    /// On success the chip is hard-reset by esptool's `--after hard_reset`,
    /// matching the post-flash behavior — so callers can treat a `true`
    /// return as "device is now running the requested firmware" without
    /// any extra reset.
    pub fn try_verify_deployment(&self, firmware_path: &Path, port: &str) -> Result<VerifyOutcome> {
        let args = self.build_verify_flash_args(firmware_path, port);
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        if self.verbose {
            tracing::info!("verify: {}", args.join(" "));
        }
        tracing::info!(
            "verifying {} on {} via esptool ({})",
            firmware_path.display(),
            port,
            self.chip
        );

        let result = run_command(
            &args_ref,
            None,
            None,
            // Verify is bounded: the slowest case is ~10s for a maximum
            // image. 30s gives plenty of headroom for slow USB-CDC stacks
            // and stub flasher upload retries.
            Some(std::time::Duration::from_secs(30)),
        )?;

        if result.success() {
            Ok(VerifyOutcome::Match {
                stdout: result.stdout,
                stderr: result.stderr,
            })
        } else {
            // esptool exits non-zero on a digest mismatch *and* on real
            // failures (port unreachable, stub upload error). Distinguish
            // them by looking at stderr — a true digest mismatch always
            // contains the literal "Verification failed" string.
            let combined = format!("{}\n{}", result.stdout, result.stderr);
            if combined.contains("Verification failed") || combined.contains("digest mismatch") {
                Ok(VerifyOutcome::Mismatch {
                    stdout: result.stdout,
                    stderr: result.stderr,
                })
            } else {
                Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "esptool verify-flash failed (exit {}): {}",
                    result.exit_code, result.stderr
                )))
            }
        }
    }
}

/// Result of a `try_verify_deployment` call.
#[derive(Debug, Clone)]
pub enum VerifyOutcome {
    /// All flashed regions match the candidate image; flashing would be
    /// a no-op. The device has been hard-reset by esptool's
    /// `--after hard_reset` so it's already running the requested image.
    Match { stdout: String, stderr: String },
    /// At least one region differs from the local files; the caller
    /// should proceed with a normal `deploy()`.
    Mismatch { stdout: String, stderr: String },
}

impl VerifyOutcome {
    /// Convenience: returns `true` only for `Match`.
    pub fn is_match(&self) -> bool {
        matches!(self, VerifyOutcome::Match { .. })
    }
}

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
/// driver name. Supported values: `esp32`, `esp32s2`, `esp32s3`.
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

fn parse_hex_offset(raw: &str) -> Result<u64> {
    let trimmed = raw.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(trimmed, 16).map_err(|e| {
        fbuild_core::FbuildError::DeployFailed(format!("invalid flash offset '{}': {}", raw, e))
    })
}

fn parse_flash_size_bytes(raw: &str) -> Result<u64> {
    let upper = raw.trim().to_ascii_uppercase();
    if let Some(num) = upper.strip_suffix("MB") {
        return num
            .trim()
            .parse::<u64>()
            .map(|n| n * 1024 * 1024)
            .map_err(|e| {
                fbuild_core::FbuildError::DeployFailed(format!(
                    "invalid flash size '{}': {}",
                    raw, e
                ))
            });
    }
    if let Some(num) = upper.strip_suffix("KB") {
        return num.trim().parse::<u64>().map(|n| n * 1024).map_err(|e| {
            fbuild_core::FbuildError::DeployFailed(format!("invalid flash size '{}': {}", raw, e))
        });
    }
    Err(fbuild_core::FbuildError::DeployFailed(format!(
        "unsupported flash size label '{}'",
        raw
    )))
}

const ESP_IMAGE_HEADER_LEN: usize = 24;
const ESP_IMAGE_SEGMENT_HEADER_LEN: usize = 8;
const ESP_IMAGE_HEADER_MAGIC: u8 = 0xE9;
const ESP_ROM_CHECKSUM_INITIAL: u32 = 0xEF;
const ESP_IMAGE_APPENDED_HASH_LEN: usize = 32;
const QEMU_ADC_CALIBRATION_SYMBOL: &str = "adc_hw_calibration";
const QEMU_ADC_CALIBRATION_PATCH_OFFSET: u32 = 3;
const QEMU_ADC_CALIBRATION_EXPECTED_BYTES: [u8; 2] = [0x0C, 0x0A];
const QEMU_ADC_CALIBRATION_PATCH_BYTES: [u8; 2] = [0x1D, 0xF0];

fn patch_qemu_esp32s3_adc_calibration(
    flash_image_path: &Path,
    firmware_path: &Path,
    elf_path: &Path,
    firmware_offset: u64,
) -> Result<()> {
    let symbol_addr = resolve_local_elf_symbol_address(elf_path, QEMU_ADC_CALIBRATION_SYMBOL)?;
    let patch_addr = symbol_addr
        .checked_add(QEMU_ADC_CALIBRATION_PATCH_OFFSET)
        .ok_or_else(|| {
            fbuild_core::FbuildError::DeployFailed(format!(
                "QEMU workaround address overflow for symbol {}",
                QEMU_ADC_CALIBRATION_SYMBOL
            ))
        })?;
    let mut firmware_bytes = std::fs::read(firmware_path)?;
    let firmware_file_offset = resolve_esp_image_file_offset(&firmware_bytes, patch_addr)?;
    patch_bytes(
        &mut firmware_bytes,
        firmware_file_offset,
        &QEMU_ADC_CALIBRATION_EXPECTED_BYTES,
        &QEMU_ADC_CALIBRATION_PATCH_BYTES,
    )?;
    repair_esp_image_checksum_and_hash(&mut firmware_bytes)?;

    let mut flash_image = std::fs::OpenOptions::new()
        .write(true)
        .open(flash_image_path)?;
    flash_image.seek(SeekFrom::Start(firmware_offset))?;
    flash_image.write_all(&firmware_bytes)?;
    tracing::info!("patched ESP32-S3 QEMU image to skip adc_hw_calibration at 0x{patch_addr:08x}");
    Ok(())
}

fn resolve_local_elf_symbol_address(elf_path: &Path, symbol_name: &str) -> Result<u32> {
    let bytes = std::fs::read(elf_path)?;
    let object = object::File::parse(bytes.as_slice()).map_err(|e| {
        fbuild_core::FbuildError::DeployFailed(format!(
            "failed to parse ELF {}: {}",
            elf_path.display(),
            e
        ))
    })?;

    let symbol = object
        .symbols()
        .find(|symbol| symbol.name().ok() == Some(symbol_name))
        .ok_or_else(|| {
            fbuild_core::FbuildError::DeployFailed(format!(
                "required ELF symbol '{}' not found in {}",
                symbol_name,
                elf_path.display()
            ))
        })?;

    u32::try_from(symbol.address()).map_err(|_| {
        fbuild_core::FbuildError::DeployFailed(format!(
            "ELF symbol '{}' address 0x{:x} does not fit in u32",
            symbol_name,
            symbol.address()
        ))
    })
}

fn resolve_esp_image_file_offset(firmware_bin: &[u8], load_addr: u32) -> Result<usize> {
    if firmware_bin.len() < ESP_IMAGE_HEADER_LEN {
        return Err(fbuild_core::FbuildError::DeployFailed(
            "firmware.bin is too small to contain an ESP image header".to_string(),
        ));
    }
    if firmware_bin[0] != ESP_IMAGE_HEADER_MAGIC {
        return Err(fbuild_core::FbuildError::DeployFailed(format!(
            "firmware.bin does not start with ESP image magic 0x{:02x}",
            ESP_IMAGE_HEADER_MAGIC
        )));
    }

    let segment_count = firmware_bin[1] as usize;
    let mut cursor = ESP_IMAGE_HEADER_LEN;
    for _ in 0..segment_count {
        if cursor + ESP_IMAGE_SEGMENT_HEADER_LEN > firmware_bin.len() {
            return Err(fbuild_core::FbuildError::DeployFailed(
                "firmware.bin ended before segment header".to_string(),
            ));
        }
        let seg_load_addr =
            u32::from_le_bytes(firmware_bin[cursor..cursor + 4].try_into().unwrap());
        let seg_len =
            u32::from_le_bytes(firmware_bin[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let data_start = cursor + ESP_IMAGE_SEGMENT_HEADER_LEN;
        let data_end = data_start + seg_len;
        if data_end > firmware_bin.len() {
            return Err(fbuild_core::FbuildError::DeployFailed(
                "firmware.bin ended before segment payload".to_string(),
            ));
        }
        let seg_end_addr = seg_load_addr.checked_add(seg_len as u32).ok_or_else(|| {
            fbuild_core::FbuildError::DeployFailed("ESP image segment address overflow".to_string())
        })?;
        if (seg_load_addr..seg_end_addr).contains(&load_addr) {
            return Ok(data_start + (load_addr - seg_load_addr) as usize);
        }
        cursor = data_end;
    }

    Err(fbuild_core::FbuildError::DeployFailed(format!(
        "firmware.bin does not contain a segment covering 0x{load_addr:08x}"
    )))
}

fn patch_bytes(bytes: &mut [u8], offset: usize, expected: &[u8], replacement: &[u8]) -> Result<()> {
    if expected.len() != replacement.len() {
        return Err(fbuild_core::FbuildError::DeployFailed(
            "patch replacement length mismatch".to_string(),
        ));
    }

    let end = offset.checked_add(expected.len()).ok_or_else(|| {
        fbuild_core::FbuildError::DeployFailed("patch offset overflow".to_string())
    })?;
    if end > bytes.len() {
        return Err(fbuild_core::FbuildError::DeployFailed(format!(
            "patch range 0x{:x}..0x{:x} exceeds image size {}",
            offset,
            end,
            bytes.len()
        )));
    }
    let actual = &bytes[offset..end];
    if actual != expected {
        return Err(fbuild_core::FbuildError::DeployFailed(format!(
            "QEMU workaround expected bytes {:02x?} at 0x{:x}, found {:02x?}",
            expected, offset, actual
        )));
    }
    bytes[offset..end].copy_from_slice(replacement);
    Ok(())
}

fn repair_esp_image_checksum_and_hash(image: &mut [u8]) -> Result<()> {
    if image.len() < ESP_IMAGE_HEADER_LEN {
        return Err(fbuild_core::FbuildError::DeployFailed(
            "firmware.bin is too small to repair".to_string(),
        ));
    }
    if image[0] != ESP_IMAGE_HEADER_MAGIC {
        return Err(fbuild_core::FbuildError::DeployFailed(format!(
            "firmware.bin does not start with ESP image magic 0x{:02x}",
            ESP_IMAGE_HEADER_MAGIC
        )));
    }

    let segment_count = image[1] as usize;
    let hash_appended = image[23] != 0;
    let mut checksum_word = ESP_ROM_CHECKSUM_INITIAL;
    let mut cursor = ESP_IMAGE_HEADER_LEN;
    for _ in 0..segment_count {
        if cursor + ESP_IMAGE_SEGMENT_HEADER_LEN > image.len() {
            return Err(fbuild_core::FbuildError::DeployFailed(
                "firmware.bin ended before segment header".to_string(),
            ));
        }
        let seg_len =
            u32::from_le_bytes(image[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let data_start = cursor + ESP_IMAGE_SEGMENT_HEADER_LEN;
        let data_end = data_start + seg_len;
        if data_end > image.len() {
            return Err(fbuild_core::FbuildError::DeployFailed(
                "firmware.bin ended before segment payload".to_string(),
            ));
        }
        for chunk in image[data_start..data_end].chunks(4) {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            checksum_word ^= u32::from_le_bytes(word);
        }
        cursor = data_end;
    }

    let checksum_block_len = ((cursor + 1 + 15) & !15) - cursor;
    let checksum_offset = cursor + checksum_block_len - 1;
    if checksum_offset >= image.len() {
        return Err(fbuild_core::FbuildError::DeployFailed(
            "firmware.bin ended before checksum byte".to_string(),
        ));
    }
    image[checksum_offset] =
        ((checksum_word >> 24) ^ (checksum_word >> 16) ^ (checksum_word >> 8) ^ checksum_word)
            as u8;

    if hash_appended {
        let hash_start = checksum_offset + 1;
        let hash_end = hash_start + ESP_IMAGE_APPENDED_HASH_LEN;
        if hash_end > image.len() {
            return Err(fbuild_core::FbuildError::DeployFailed(
                "firmware.bin ended before appended hash".to_string(),
            ));
        }
        let digest = Sha256::digest(&image[..hash_start]);
        image[hash_start..hash_end].copy_from_slice(&digest);
    }
    Ok(())
}

fn fill_with_ff(file: &mut std::fs::File, total_size: u64) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let chunk = vec![0xFFu8; 64 * 1024];
    let mut remaining = total_size;
    while remaining > 0 {
        let to_write = std::cmp::min(remaining, chunk.len() as u64) as usize;
        file.write_all(&chunk[..to_write])?;
        remaining -= to_write as u64;
    }
    Ok(())
}

fn write_binary_at_offset(
    output: &mut std::fs::File,
    input_path: &Path,
    offset: u64,
    flash_size_bytes: u64,
) -> Result<()> {
    let metadata = std::fs::metadata(input_path)?;
    let end = offset.saturating_add(metadata.len());
    if end > flash_size_bytes {
        return Err(fbuild_core::FbuildError::DeployFailed(format!(
            "artifact {} at offset 0x{:x} exceeds flash image size {}",
            input_path.display(),
            offset,
            flash_size_bytes
        )));
    }

    output.seek(SeekFrom::Start(offset))?;
    let mut input = std::fs::File::open(input_path)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
    }
    Ok(())
}

impl Deployer for Esp32Deployer {
    fn deploy(
        &self,
        project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult> {
        let port = port.ok_or_else(|| {
            fbuild_core::FbuildError::DeployFailed(
                "serial port required for ESP32 deploy (use --port)".to_string(),
            )
        })?;

        let build_dir = firmware_path.parent().unwrap_or(project_dir);
        let bootloader_path = build_dir.join("bootloader.bin");
        let partitions_path = build_dir.join("partitions.bin");

        let mut args = Self::find_esptool();

        // Chip and port
        args.extend([
            "--chip".to_string(),
            self.chip.clone(),
            "--port".to_string(),
            port.to_string(),
            "--baud".to_string(),
            self.baud_rate.clone(),
        ]);

        // Reset behavior
        args.extend([
            "--before".to_string(),
            self.before_reset.clone(),
            "--after".to_string(),
            self.after_reset.clone(),
        ]);

        // Write flash command
        args.extend([
            "write_flash".to_string(),
            "-z".to_string(),
            "--flash-mode".to_string(),
            self.flash_mode.clone(),
            "--flash-freq".to_string(),
            self.flash_freq.clone(),
            "--flash-size".to_string(),
            "detect".to_string(),
        ]);

        // Flash addresses and files
        if bootloader_path.exists() {
            args.push(self.bootloader_offset.clone());
            args.push(bootloader_path.to_string_lossy().to_string());
        }

        if partitions_path.exists() {
            args.push(self.partitions_offset.clone());
            args.push(partitions_path.to_string_lossy().to_string());
        }

        args.push(self.firmware_offset.clone());
        args.push(firmware_path.to_string_lossy().to_string());

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        if self.verbose {
            tracing::info!("deploy: {}", args.join(" "));
        }

        tracing::info!(
            "flashing {} to {} via esptool ({})",
            firmware_path.display(),
            port,
            self.chip
        );

        let result = run_command(
            &args_ref,
            None,
            None,
            Some(std::time::Duration::from_secs(120)),
        )?;

        if result.success() {
            Ok(DeploymentResult {
                success: true,
                message: format!("firmware flashed to {} ({})", port, self.chip),
                port: Some(port.to_string()),
                stdout: result.stdout,
                stderr: result.stderr,
            })
        } else {
            // Return a non-success DeploymentResult instead of Err so the
            // daemon handler can forward esptool's stdout/stderr to the client.
            Ok(DeploymentResult {
                success: false,
                message: format!("esptool failed (exit code {})", result.exit_code),
                port: Some(port.to_string()),
                stdout: result.stdout,
                stderr: result.stderr,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test params matching ESP32-C6 JSON config values.
    fn test_esptool_params() -> EsptoolParams {
        EsptoolParams {
            flash_mode: "dio".to_string(),
            flash_freq: "80m".to_string(),
            default_baud: "460800".to_string(),
            before_reset: "default_reset".to_string(),
            after_reset: "hard_reset".to_string(),
        }
    }

    #[test]
    fn test_esp32_deployer_creation() {
        let params = test_esptool_params();
        let deployer = Esp32Deployer::new(
            "esp32c6", "460800", "0x0", "0x8000", "0x10000", &params, false,
        );
        assert_eq!(deployer.chip, "esp32c6");
        assert_eq!(deployer.baud_rate, "460800");
        assert_eq!(deployer.bootloader_offset, "0x0");
        assert_eq!(deployer.firmware_offset, "0x10000");
        assert_eq!(deployer.flash_mode, "dio");
        assert_eq!(deployer.before_reset, "default_reset");
    }

    #[test]
    fn qemu_flash_size_resolution_accepts_supported_sizes() {
        let mut board =
            fbuild_config::BoardConfig::from_board_id("esp32-s3-devkitc-1", &Default::default())
                .unwrap();
        board.max_flash = Some(8 * 1024 * 1024);
        assert_eq!(
            resolve_qemu_flash_size_bytes(&board, "4MB").unwrap(),
            8 * 1024 * 1024
        );
    }

    #[test]
    fn qemu_flash_size_resolution_rejects_unsupported_size() {
        let mut board =
            fbuild_config::BoardConfig::from_board_id("esp32-s3-devkitc-1", &Default::default())
                .unwrap();
        board.max_flash = Some(32 * 1024 * 1024);
        let err = resolve_qemu_flash_size_bytes(&board, "4MB").unwrap_err();
        assert!(err
            .to_string()
            .contains("supports only 2MB, 4MB, 8MB, or 16MB"));
    }

    #[test]
    fn create_qemu_flash_image_writes_regions_at_offsets() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        std::fs::create_dir_all(&build_dir).unwrap();

        let boot = build_dir.join("bootloader.bin");
        let parts = build_dir.join("partitions.bin");
        let fw = build_dir.join("firmware.bin");
        std::fs::write(&boot, b"BOOT").unwrap();
        std::fs::write(&parts, b"PART").unwrap();
        std::fs::write(&fw, b"FIRM").unwrap();

        let flash = tmp.path().join("qemu_flash.bin");
        create_qemu_flash_image(
            &fw,
            &flash,
            2 * 1024 * 1024,
            "0x0",
            "0x8000",
            "0x10000",
            None,
        )
        .unwrap();

        let bytes = std::fs::read(&flash).unwrap();
        assert_eq!(&bytes[0..4], b"BOOT");
        assert_eq!(&bytes[0x8000..0x8004], b"PART");
        assert_eq!(&bytes[0x10000..0x10004], b"FIRM");
        assert_eq!(bytes.len(), 2 * 1024 * 1024);
        assert_eq!(bytes[0x200], 0xFF);
    }

    #[test]
    fn create_qemu_flash_image_includes_boot_app0_when_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        std::fs::create_dir_all(&build_dir).unwrap();

        let boot = build_dir.join("bootloader.bin");
        let boot_app0 = build_dir.join("boot_app0.bin");
        let parts = build_dir.join("partitions.bin");
        let fw = build_dir.join("firmware.bin");
        std::fs::write(&boot, b"BOOT").unwrap();
        std::fs::write(&boot_app0, b"APP0").unwrap();
        std::fs::write(&parts, b"PART").unwrap();
        std::fs::write(&fw, b"FIRM").unwrap();

        let flash = tmp.path().join("qemu_flash.bin");
        create_qemu_flash_image(
            &fw,
            &flash,
            2 * 1024 * 1024,
            "0x0",
            "0x8000",
            "0x10000",
            None,
        )
        .unwrap();

        let bytes = std::fs::read(&flash).unwrap();
        assert_eq!(&bytes[0xE000..0xE004], b"APP0");
    }

    #[test]
    fn resolve_esp_image_file_offset_maps_address_into_segment_data() {
        let mut image = vec![0u8; ESP_IMAGE_HEADER_LEN];
        image[0] = ESP_IMAGE_HEADER_MAGIC;
        image[1] = 1;
        image.extend_from_slice(&0x4200_0000u32.to_le_bytes());
        image.extend_from_slice(&6u32.to_le_bytes());
        image.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);

        let offset = resolve_esp_image_file_offset(&image, 0x4200_0003).unwrap();
        assert_eq!(
            offset,
            ESP_IMAGE_HEADER_LEN + ESP_IMAGE_SEGMENT_HEADER_LEN + 3
        );
    }

    #[test]
    fn patch_bytes_rewrites_expected_bytes_only() {
        let mut flash = [0xFFu8; 16];
        flash[6..8].copy_from_slice(&QEMU_ADC_CALIBRATION_EXPECTED_BYTES);

        patch_bytes(
            &mut flash,
            6,
            &QEMU_ADC_CALIBRATION_EXPECTED_BYTES,
            &QEMU_ADC_CALIBRATION_PATCH_BYTES,
        )
        .unwrap();

        assert_eq!(&flash[6..8], &QEMU_ADC_CALIBRATION_PATCH_BYTES);
    }

    #[test]
    fn repair_esp_image_checksum_and_hash_updates_trailers_after_patch() {
        let mut image = vec![0u8; ESP_IMAGE_HEADER_LEN];
        image[0] = ESP_IMAGE_HEADER_MAGIC;
        image[1] = 1;
        image[23] = 1;
        image.extend_from_slice(&0x4200_0000u32.to_le_bytes());
        image.extend_from_slice(&8u32.to_le_bytes());
        image.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        image.extend_from_slice(&[0u8; 16]);
        image.extend_from_slice(&[0u8; ESP_IMAGE_APPENDED_HASH_LEN]);

        patch_bytes(
            &mut image,
            ESP_IMAGE_HEADER_LEN + ESP_IMAGE_SEGMENT_HEADER_LEN + 3,
            &[4],
            &[9],
        )
        .unwrap();
        repair_esp_image_checksum_and_hash(&mut image).unwrap();

        let checksum_offset =
            ((ESP_IMAGE_HEADER_LEN + ESP_IMAGE_SEGMENT_HEADER_LEN + 8 + 1 + 15) & !15) - 1;
        let expected_checksum = {
            let mut checksum_word = ESP_ROM_CHECKSUM_INITIAL;
            for chunk in image[ESP_IMAGE_HEADER_LEN + ESP_IMAGE_SEGMENT_HEADER_LEN
                ..ESP_IMAGE_HEADER_LEN + ESP_IMAGE_SEGMENT_HEADER_LEN + 8]
                .chunks(4)
            {
                let mut word = [0u8; 4];
                word[..chunk.len()].copy_from_slice(chunk);
                checksum_word ^= u32::from_le_bytes(word);
            }
            ((checksum_word >> 24) ^ (checksum_word >> 16) ^ (checksum_word >> 8) ^ checksum_word)
                as u8
        };
        let expected_hash = Sha256::digest(&image[..checksum_offset + 1]);
        assert_eq!(image[checksum_offset], expected_checksum);
        assert_eq!(
            &image[checksum_offset + 1..checksum_offset + 1 + ESP_IMAGE_APPENDED_HASH_LEN],
            expected_hash.as_slice()
        );
    }

    #[test]
    fn qemu_command_builder_uses_expected_machine_and_watchdog_override() {
        let args = build_qemu_esp32s3_args(Path::new("flash.bin"), None);
        assert!(args.contains(&"esp32s3".to_string()));
        assert!(args
            .iter()
            .any(|arg| arg == "driver=timer.esp32s3.timg,property=wdt_disable,value=true"));
        assert!(args
            .iter()
            .any(|arg| arg.contains("file=flash.bin,if=mtd,format=raw")));
    }

    #[test]
    fn qemu_command_builder_uses_esp32_machine_for_base_variant() {
        let args = build_qemu_args("esp32", Path::new("flash.bin"), None);
        assert!(args.contains(&"esp32".to_string()));
        assert!(args
            .iter()
            .any(|arg| arg == "driver=timer.esp32.timg,property=wdt_disable,value=true"));
        assert!(args
            .iter()
            .any(|arg| arg.contains("file=flash.bin,if=mtd,format=raw")));
    }

    #[test]
    fn qemu_command_builder_adds_psram_args_when_requested() {
        let args = build_qemu_esp32s3_args(
            Path::new("flash.bin"),
            Some(fbuild_config::Esp32QemuPsramConfig {
                size_mib: 8,
                is_octal: true,
            }),
        );
        assert!(args.windows(2).any(|pair| pair == ["-m", "8M"]));
        assert!(args
            .iter()
            .any(|arg| arg == "driver=ssi_psram,property=is_octal,value=true"));
    }

    #[test]
    fn test_esp32_deployer_from_board_config() {
        let board =
            fbuild_config::BoardConfig::from_board_id("esp32c6", &std::collections::HashMap::new())
                .unwrap();
        let params = test_esptool_params();
        let deployer =
            Esp32Deployer::from_board_config(&board, "0x0", "0x8000", "0x10000", &params, false);
        assert_eq!(deployer.chip, "esp32c6");
        assert_eq!(deployer.bootloader_offset, "0x0");
    }

    #[test]
    fn test_deploy_requires_port() {
        let params = test_esptool_params();
        let deployer = Esp32Deployer::new(
            "esp32c6", "460800", "0x0", "0x8000", "0x10000", &params, false,
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let result = deployer.deploy(tmp.path(), "esp32c6", Path::new("firmware.bin"), None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("serial port required"));
    }

    /// Fast deploy: the verify-flash command line must include the
    /// `verify-flash` subcommand and pair every flash region with its
    /// matching offset, in the order bootloader, partitions, firmware.
    /// Verifying all three in a single esptool call amortises the
    /// ~3-second stub flasher upload.
    #[test]
    fn build_verify_flash_args_includes_all_three_regions_when_present() {
        let params = test_esptool_params();
        let deployer = Esp32Deployer::new(
            "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
        );
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("bootloader.bin"), b"boot").unwrap();
        std::fs::write(tmp.path().join("partitions.bin"), b"part").unwrap();
        let fw = tmp.path().join("firmware.bin");
        std::fs::write(&fw, b"firm").unwrap();

        let args = deployer.build_verify_flash_args(&fw, "COM13");

        // Subcommand
        assert!(
            args.contains(&"verify-flash".to_string()),
            "missing verify-flash subcommand: {:?}",
            args
        );
        // Chip + port
        assert!(args.contains(&"--chip".to_string()));
        assert!(args.contains(&"esp32s3".to_string()));
        assert!(args.contains(&"--port".to_string()));
        assert!(args.contains(&"COM13".to_string()));

        // All three (offset, file) pairs in the right order.
        let pos_verify = args.iter().position(|a| a == "verify-flash").unwrap();
        let pos_boot = args.iter().position(|a| a == "0x0").unwrap();
        let pos_parts = args.iter().position(|a| a == "0x8000").unwrap();
        let pos_fw = args.iter().position(|a| a == "0x10000").unwrap();
        assert!(
            pos_verify < pos_boot && pos_boot < pos_parts && pos_parts < pos_fw,
            "regions must appear after verify-flash in bootloader→partitions→firmware order: {:?}",
            args
        );

        // verify-flash MUST NOT carry --flash-mode/freq/size flags;
        // those are write-flash options and esptool 5.x rejects them
        // here. We were burned by this when we copied the deploy()
        // argument layout wholesale.
        assert!(
            !args.contains(&"--flash-mode".to_string()),
            "verify-flash must not include --flash-mode (write-flash only): {:?}",
            args
        );
        assert!(
            !args.contains(&"--flash-freq".to_string()),
            "verify-flash must not include --flash-freq (write-flash only): {:?}",
            args
        );
    }

    #[test]
    fn build_verify_flash_args_skips_missing_bootloader_and_partitions() {
        // When bootloader.bin / partitions.bin haven't been built (e.g.
        // an upload-only test fixture), verify must still cover firmware
        // alone. Otherwise we'd skip the only thing we have.
        let params = test_esptool_params();
        let deployer = Esp32Deployer::new(
            "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = tmp.path().join("firmware.bin");
        std::fs::write(&fw, b"firm").unwrap();

        let args = deployer.build_verify_flash_args(&fw, "COM13");

        // No bootloader or partitions paths in the args.
        assert!(!args.iter().any(|a| a.ends_with("bootloader.bin")));
        assert!(!args.iter().any(|a| a.ends_with("partitions.bin")));
        // Firmware offset is still present.
        assert!(args.contains(&"0x10000".to_string()));
        assert!(args.iter().any(|a| a.ends_with("firmware.bin")));
    }

    #[test]
    fn verify_outcome_is_match_helper() {
        let m = VerifyOutcome::Match {
            stdout: "ok".into(),
            stderr: String::new(),
        };
        let mm = VerifyOutcome::Mismatch {
            stdout: String::new(),
            stderr: "Verification failed".into(),
        };
        assert!(m.is_match());
        assert!(!mm.is_match());
    }

    // ---------------------------------------------------------------
    // Hardware-gated verify-deployment tests for each ESP32 family MCU.
    //
    // These tests are `#[ignore]` so they never run in CI.  To exercise
    // them on a local bench, set the env vars described below and run:
    //
    //   uv run cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real -- --ignored --nocapture
    //
    // Each test reads **two** environment variables:
    //
    //   <MCU>_PORT        – serial port the board is attached to (e.g. COM13, /dev/ttyUSB0)
    //   <MCU>_FIRMWARE    – absolute path to a pre-flashed firmware.bin
    //
    // where <MCU> is one of ESP32, ESP32S2, ESP32S3, ESP32C2, ESP32C3,
    // ESP32C6, ESP32H2, ESP32P4.
    //
    // The firmware directory must also contain `bootloader.bin` and
    // `partitions.bin` so that verify-flash can check all three regions
    // in a single esptool invocation.
    //
    // Bootloader offsets per chip (from esp32.rs header comment):
    //   0x1000 – esp32, esp32s2
    //   0x0    – esp32c2, esp32c3, esp32c5, esp32c6, esp32h2, esp32s3
    //   0x2000 – esp32p4
    // ---------------------------------------------------------------

    /// Shared implementation for all per-chip hardware-gated verify tests.
    ///
    /// 1. Reads `{port_env}` and `{firmware_env}` from the environment.
    /// 2. Asserts that verify against the pre-flashed image returns `Match`
    ///    in under 15 seconds.
    /// 3. Asserts that a tampered image (1 byte flipped) returns `Mismatch`.
    fn run_verify_deployment_test(
        chip: &str,
        bootloader_offset: &str,
        port_env: &str,
        firmware_env: &str,
    ) {
        let port = std::env::var(port_env).unwrap_or_else(|_| {
            panic!(
                "set {} to the serial port your {} board is attached to (e.g. COM13)",
                port_env, chip
            )
        });
        let firmware_path = std::env::var(firmware_env).unwrap_or_else(|_| {
            panic!(
                "set {} to the absolute path of the pre-flashed firmware.bin for {}",
                firmware_env, chip
            )
        });
        let reference = std::path::PathBuf::from(&firmware_path);
        if !reference.exists() {
            panic!(
                "reference firmware not found at {}; build and flash it first",
                reference.display()
            );
        }

        let params = EsptoolParams {
            flash_mode: "dio".to_string(),
            flash_freq: "80m".to_string(),
            default_baud: "921600".to_string(),
            before_reset: "default_reset".to_string(),
            after_reset: "hard_reset".to_string(),
        };
        let deployer = Esp32Deployer::new(
            chip,
            "921600",
            bootloader_offset,
            "0x8000",
            "0x10000",
            &params,
            true,
        );

        // Phase 1: matching image -> Match
        let start = std::time::Instant::now();
        let outcome = deployer
            .try_verify_deployment(&reference, &port)
            .unwrap_or_else(|e| panic!("verify must not fail against attached {}: {}", chip, e));
        let elapsed = start.elapsed();
        assert!(
            outcome.is_match(),
            "[{}] expected Match against pre-flashed firmware; got {:?}",
            chip,
            outcome
        );
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "[{}] verify took {:?} -- should complete in <15s",
            chip,
            elapsed
        );
        eprintln!("[{}] verify (Match) elapsed: {:?}", chip, elapsed);

        // Phase 2: tampered image -> Mismatch
        let tmp = tempfile::TempDir::new().unwrap();
        let ref_dir = reference.parent().unwrap();
        // Copy bootloader and partitions next to the tampered firmware so
        // build_verify_flash_args picks them up alongside firmware.bin.
        for name in &["bootloader.bin", "partitions.bin"] {
            let src = ref_dir.join(name);
            if src.exists() {
                std::fs::copy(&src, tmp.path().join(name)).unwrap();
            }
        }
        let tampered = tmp.path().join("firmware.bin");
        let mut bytes = std::fs::read(&reference).unwrap();
        // Flip a byte well past the image header to avoid invalidating
        // the ESP-IDF magic and triggering an esptool parse error rather
        // than a clean digest mismatch.
        let target = bytes.len() / 2;
        bytes[target] ^= 0x55;
        std::fs::write(&tampered, &bytes).unwrap();

        let outcome = deployer
            .try_verify_deployment(&tampered, &port)
            .unwrap_or_else(|e| {
                panic!(
                    "[{}] verify must not fail with tampered firmware: {}",
                    chip, e
                )
            });
        assert!(
            !outcome.is_match(),
            "[{}] expected Mismatch for tampered firmware; got {:?}",
            chip,
            outcome
        );
        eprintln!("[{}] verify (Mismatch) detected correctly", chip);
    }

    /// ESP32 (Xtensa, bootloader at 0x1000).
    ///
    /// ```text
    /// ESP32_PORT=COM5 ESP32_FIRMWARE=C:\path\to\firmware.bin \
    ///   uv run cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32 -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires real ESP32 board — set ESP32_PORT and ESP32_FIRMWARE"]
    fn try_verify_deployment_real_esp32() {
        run_verify_deployment_test("esp32", "0x1000", "ESP32_PORT", "ESP32_FIRMWARE");
    }

    /// ESP32-S2 (Xtensa single-core, bootloader at 0x1000).
    ///
    /// ```text
    /// ESP32S2_PORT=COM6 ESP32S2_FIRMWARE=C:\path\to\firmware.bin \
    ///   uv run cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32s2 -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires real ESP32-S2 board — set ESP32S2_PORT and ESP32S2_FIRMWARE"]
    fn try_verify_deployment_real_esp32s2() {
        run_verify_deployment_test("esp32s2", "0x1000", "ESP32S2_PORT", "ESP32S2_FIRMWARE");
    }

    /// ESP32-S3 (Xtensa dual-core, bootloader at 0x0).
    ///
    /// This is the original baseline test, now using env-var configuration
    /// consistent with the rest of the family.
    ///
    /// ```text
    /// ESP32S3_PORT=COM13 ESP32S3_FIRMWARE=C:\Users\niteris\dev\fastled\.pio\build\esp32s3\firmware.bin \
    ///   uv run cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32s3 -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires real ESP32-S3 board — set ESP32S3_PORT and ESP32S3_FIRMWARE"]
    fn try_verify_deployment_real_esp32s3() {
        run_verify_deployment_test("esp32s3", "0x0", "ESP32S3_PORT", "ESP32S3_FIRMWARE");
    }

    /// ESP32-C2 (RISC-V single-core, bootloader at 0x0).
    ///
    /// ```text
    /// ESP32C2_PORT=COM7 ESP32C2_FIRMWARE=C:\path\to\firmware.bin \
    ///   uv run cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32c2 -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires real ESP32-C2 board — set ESP32C2_PORT and ESP32C2_FIRMWARE"]
    fn try_verify_deployment_real_esp32c2() {
        run_verify_deployment_test("esp32c2", "0x0", "ESP32C2_PORT", "ESP32C2_FIRMWARE");
    }

    /// ESP32-C3 (RISC-V single-core, bootloader at 0x0).
    ///
    /// ```text
    /// ESP32C3_PORT=COM8 ESP32C3_FIRMWARE=C:\path\to\firmware.bin \
    ///   uv run cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32c3 -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires real ESP32-C3 board — set ESP32C3_PORT and ESP32C3_FIRMWARE"]
    fn try_verify_deployment_real_esp32c3() {
        run_verify_deployment_test("esp32c3", "0x0", "ESP32C3_PORT", "ESP32C3_FIRMWARE");
    }

    /// ESP32-C6 (RISC-V single-core, bootloader at 0x0).
    ///
    /// ```text
    /// ESP32C6_PORT=COM9 ESP32C6_FIRMWARE=C:\path\to\firmware.bin \
    ///   uv run cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32c6 -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires real ESP32-C6 board — set ESP32C6_PORT and ESP32C6_FIRMWARE"]
    fn try_verify_deployment_real_esp32c6() {
        run_verify_deployment_test("esp32c6", "0x0", "ESP32C6_PORT", "ESP32C6_FIRMWARE");
    }

    /// ESP32-H2 (RISC-V single-core, bootloader at 0x0).
    ///
    /// ```text
    /// ESP32H2_PORT=COM10 ESP32H2_FIRMWARE=C:\path\to\firmware.bin \
    ///   uv run cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32h2 -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires real ESP32-H2 board — set ESP32H2_PORT and ESP32H2_FIRMWARE"]
    fn try_verify_deployment_real_esp32h2() {
        run_verify_deployment_test("esp32h2", "0x0", "ESP32H2_PORT", "ESP32H2_FIRMWARE");
    }

    /// ESP32-P4 (RISC-V dual-core, OPI flash, bootloader at 0x2000).
    ///
    /// Note: ESP32-P4 uses OPI flash and has a different bootloader offset
    /// (0x2000) compared to other ESP32 chips.
    ///
    /// ```text
    /// ESP32P4_PORT=COM11 ESP32P4_FIRMWARE=C:\path\to\firmware.bin \
    ///   uv run cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32p4 -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires real ESP32-P4 board — set ESP32P4_PORT and ESP32P4_FIRMWARE"]
    fn try_verify_deployment_real_esp32p4() {
        run_verify_deployment_test("esp32p4", "0x2000", "ESP32P4_PORT", "ESP32P4_FIRMWARE");
    }
}
