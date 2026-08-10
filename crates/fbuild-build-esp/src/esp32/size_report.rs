//! ESP32 section-size reporting using SysV format (`size -A`).
//!
//! The default Berkeley `size` format lumps `.flash.rodata` (flash-resident)
//! into the `data` column alongside `.dram0.data` (RAM-resident), inflating
//! the RAM figure — for ESP32-C6 this can report 602% RAM usage
//! (FastLED/fbuild#1261).
//!
//! SysV format lists each section individually with its name and address,
//! so flash and RAM sections can be classified by prefix.

use std::path::Path;

use fbuild_core::SizeInfo;
use fbuild_core::subprocess::run_command;

/// Run `size -A` and parse the SysV output for an ESP32 ELF.
///
/// Falls back to `None` when no `.flash.*` / `.dram0.*` sections are
/// detected, so the caller can fall through to the standard Berkeley parser.
pub(crate) async fn esp32_report_size(
    size_path: &Path,
    elf_path: &Path,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
) -> fbuild_core::Result<SizeInfo> {
    let args = [
        size_path.to_string_lossy().to_string(),
        "-A".to_string(),
        elf_path.to_string_lossy().to_string(),
    ];
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let result = run_command(
        &args_ref,
        None,
        None,
        Some(std::time::Duration::from_secs(15)),
    )
    .await?;

    if !result.success() {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "size -A failed: {}",
            result.stderr
        )));
    }

    parse_esp32_size_sysv(&result.stdout, max_flash, max_ram).ok_or_else(|| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to parse ESP32 size -A output:\n{}",
            result.stdout
        ))
    })
}

/// Parse `size -A` (SysV format) output for ESP32 targets.
///
/// ```text
/// section             size         addr
/// .flash.text        89012    0x42000020
/// .flash.rodata      45678    0x42015c34
/// .dram0.data         1234    0x3fc80000
/// .dram0.bss          5678    0x3fc81234
/// .iram0.text          789    0x40800000
/// Total              142391
/// ```
///
/// Classification:
/// - Flash: `.flash.*`, `.rodata*`, `.iram0.*`, `.text`
/// - RAM:   `.dram0.*`, `.dram.*`
///
/// Returns `None` when no ESP32-prefixed sections are detected.
pub(crate) fn parse_esp32_size_sysv(
    output: &str,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
) -> Option<SizeInfo> {
    let mut flash: u64 = 0;
    let mut ram_data: u64 = 0;
    let mut ram_bss: u64 = 0;
    let mut has_esp_sections = false;

    for line in output.lines() {
        if line.starts_with("section") || line.starts_with("Total") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let section = parts[0];
        let Ok(size) = parts[1].parse::<u64>() else {
            continue;
        };

        if section.starts_with(".flash.") || section.starts_with(".rodata") {
            flash += size;
            has_esp_sections = true;
        } else if section.starts_with(".dram0.") || section.starts_with(".dram.") {
            if section.ends_with(".bss") || section.contains(".bss") {
                ram_bss += size;
            } else {
                ram_data += size;
            }
            has_esp_sections = true;
        } else if section.starts_with(".iram0.") || section.starts_with(".iram.") {
            flash += size;
            has_esp_sections = true;
        } else if section == ".text" {
            flash += size;
        } else if section == ".data" {
            ram_data += size;
        } else if section == ".bss" {
            ram_bss += size;
        }
    }

    if !has_esp_sections {
        return None;
    }

    Some(SizeInfo {
        text: flash,
        data: ram_data,
        bss: ram_bss,
        total_flash: flash + ram_data,
        total_ram: ram_data + ram_bss,
        max_flash,
        max_ram,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysv_separates_flash_from_ram() {
        // Simulates `size -A` output for an ESP32-C6 build.
        // .flash.rodata (45KB in flash) must NOT inflate RAM.
        let output = "\
section             size         addr
.flash.text        89012    0x42000020
.flash.rodata      45678    0x42015c34
.dram0.data         1234    0x3fc80000
.dram0.bss          5678    0x3fc81234
.iram0.text          789    0x40800000
Total              142391
";
        let info = parse_esp32_size_sysv(output, Some(4_194_304), Some(327_680)).unwrap();
        assert_eq!(info.text, 89012 + 45678 + 789);
        assert_eq!(info.data, 1234);
        assert_eq!(info.bss, 5678);
        assert_eq!(info.total_flash, 89012 + 45678 + 789 + 1234);
        // Only dram0 sections count as RAM — no .flash.rodata contamination
        assert_eq!(info.total_ram, 1234 + 5678);
        assert!(info.ram_percent().unwrap() < 100.0);
    }

    #[test]
    fn returns_none_for_non_esp_output() {
        // Standard Berkeley output without ESP32 section prefixes
        // should return None, so the caller falls back to Berkeley parser.
        let output = "\
   text    data     bss     dec     hex filename
   1234     56      78    1368     558 firmware.elf
";
        assert!(parse_esp32_size_sysv(output, None, None).is_none());
    }

    #[test]
    fn ignores_total_and_header_lines() {
        let output = "\
section             size         addr
.dram0.data         1000    0x3fc80000
.dram0.bss          2000    0x3fc81000
.flash.text        40000    0x42000020
Total               43000
";
        let info = parse_esp32_size_sysv(output, None, None).unwrap();
        assert_eq!(info.total_flash, 40000 + 1000);
        assert_eq!(info.total_ram, 1000 + 2000);
    }
}
