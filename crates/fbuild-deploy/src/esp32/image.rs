//! ESP image header constants, patching, checksum repair, and raw binary
//! I/O helpers used when assembling a QEMU flash image.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use fbuild_core::Result;
use object::{Object, ObjectSymbol};
use sha2::{Digest, Sha256};

pub(super) const ESP_IMAGE_HEADER_LEN: usize = 24;
pub(super) const ESP_IMAGE_SEGMENT_HEADER_LEN: usize = 8;
pub(super) const ESP_IMAGE_HEADER_MAGIC: u8 = 0xE9;
pub(super) const ESP_ROM_CHECKSUM_INITIAL: u32 = 0xEF;
pub(super) const ESP_IMAGE_APPENDED_HASH_LEN: usize = 32;
pub(super) const QEMU_ADC_CALIBRATION_SYMBOL: &str = "adc_hw_calibration";
pub(super) const QEMU_ADC_CALIBRATION_PATCH_OFFSET: u32 = 3;
pub(super) const QEMU_ADC_CALIBRATION_EXPECTED_BYTES: [u8; 2] = [0x0C, 0x0A];
pub(super) const QEMU_ADC_CALIBRATION_PATCH_BYTES: [u8; 2] = [0x1D, 0xF0];

pub(super) fn patch_qemu_esp32s3_adc_calibration(
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

pub(super) fn resolve_esp_image_file_offset(firmware_bin: &[u8], load_addr: u32) -> Result<usize> {
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

pub(super) fn patch_bytes(
    bytes: &mut [u8],
    offset: usize,
    expected: &[u8],
    replacement: &[u8],
) -> Result<()> {
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

pub(super) fn repair_esp_image_checksum_and_hash(image: &mut [u8]) -> Result<()> {
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

pub(super) fn fill_with_ff(file: &mut std::fs::File, total_size: u64) -> Result<()> {
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

pub(super) fn write_binary_at_offset(
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
