//! Region-specific diagnostics for failed GNU links (#1658).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use fbuild_core::FbuildError;

/// Discard an earlier link's map before attempting a new link. If this fails,
/// the old map cannot be trusted even if the linker subsequently fails too.
pub(crate) fn prepare_map(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(
                "cannot clear previous linker map {}: {error}",
                path.display()
            );
            false
        }
    }
}

pub(crate) fn enrich(error: FbuildError, map_path: Option<&Path>) -> FbuildError {
    let FbuildError::BuildFailed(mut message) = error else {
        return error;
    };
    let overflows = overflowing_regions(&message);
    if overflows.is_empty() {
        return FbuildError::BuildFailed(message);
    }
    let capacities = map_path
        .and_then(|path| std::fs::File::open(path).ok())
        .map(|file| region_capacities(BufReader::new(file)))
        .unwrap_or_default();
    for (region, overflow) in overflows {
        let lower = region.to_ascii_lowercase();
        let label = if lower.contains("ram") || lower.contains("rtc") || lower.contains("tcm") {
            "RAM"
        } else if lower.contains("flash") || lower.contains("rom") {
            "Flash"
        } else {
            "Memory"
        };
        message.push_str(&format!(
            "\n{label}: {region} overflowed by {overflow} bytes"
        ));
        if let Some(&capacity) = capacities.get(&region) {
            message.push_str(&format!(" (region {capacity} bytes)"));
            let required = u128::from(capacity) + u128::from(overflow);
            if capacity > 0 {
                let percent = required as f64 * 100.0 / capacity as f64;
                message.push_str(&format!("; requires {required} bytes ({percent:.1}%)"));
            }
        } else {
            message.push_str(" (region size unavailable)");
        }
    }
    FbuildError::BuildFailed(message)
}

fn overflowing_regions(message: &str) -> BTreeMap<String, u64> {
    let mut regions = BTreeMap::<String, u64>::new();
    for line in message.lines() {
        let Some((_, tail)) = line.split_once("region ") else {
            continue;
        };
        let Some(quote) = tail.chars().next() else {
            continue;
        };
        let end_quote = match quote {
            '`' | '\'' => '\'',
            '"' => '"',
            _ => continue,
        };
        let Some((name, amount)) = tail[1..].split_once(end_quote) else {
            continue;
        };
        let Some(amount) = amount.strip_prefix(" overflowed by ") else {
            continue;
        };
        let Some(amount) = amount
            .strip_suffix(" bytes")
            .or_else(|| amount.strip_suffix(" bytes."))
        else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        if let Ok(amount) = amount.parse::<u64>() {
            regions
                .entry(name.to_owned())
                .and_modify(|old| *old = (*old).max(amount))
                .or_insert(amount);
        }
    }
    regions
}

fn number(value: &str) -> Option<u64> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        value.parse().ok()
    }
}

fn region_capacities(reader: impl BufRead) -> BTreeMap<String, u64> {
    let mut capacities = BTreeMap::new();
    let mut in_table = false;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim() == "Memory Configuration" {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        if line.contains("Linker script and memory map")
            || (line.trim().is_empty() && !capacities.is_empty())
        {
            break;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() >= 3 && number(fields[1]).is_some() {
            if let Some(length) = number(fields[2]) {
                capacities.insert(fields[0].to_owned(), length);
            }
        }
    }
    capacities
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_memory_table_with_windows_line_endings() {
        let map = b"ignored 0x0 999\r\nMemory Configuration\r\n\r\nName Origin Length Attributes\r\ndram0_0_seg\t0x3fc00000\t0x1e800\txrw\r\nFLASH 65536 256 xr\r\n\r\nLinker script and memory map\r\ndram0_0_seg 0x0 999\r\n";
        let capacities = region_capacities(&map[..]);
        assert_eq!(capacities.get("dram0_0_seg"), Some(&124928));
        assert_eq!(capacities.get("FLASH"), Some(&256));
        assert_eq!(capacities.len(), 2);
    }

    #[test]
    fn deduplicates_and_ignores_other_diagnostics() {
        let regions = overflowing_regions(
            "ld.exe: region `ram' overflowed by 20 bytes\nregion 'ram' overflowed by 10 bytes\nregion \"rom\" overflowed by 5 bytes\nregion `bad' overflowed by invalid bytes\nsection will not fit in region `other'",
        );
        assert_eq!(
            regions,
            BTreeMap::from([("ram".into(), 20), ("rom".into(), 5)])
        );
    }

    #[test]
    fn missing_map_never_uses_a_board_total() {
        let text = enrich(
            FbuildError::BuildFailed("region `dram0_0_seg' overflowed by 136760 bytes".into()),
            None,
        )
        .to_string();
        assert!(
            text.contains("RAM: dram0_0_seg overflowed by 136760 bytes (region size unavailable)")
        );
        assert!(!text.contains('%'));
    }

    #[test]
    fn clears_stale_maps_and_preserves_unrelated_failures() {
        let tmp = tempfile::tempdir().unwrap();
        let map = tmp.path().join("firmware.map");
        std::fs::write(&map, "stale").unwrap();
        assert!(prepare_map(&map));
        assert!(!map.exists());
        assert!(prepare_map(&map));
        assert_eq!(
            enrich(
                FbuildError::BuildFailed("undefined reference".into()),
                Some(&map)
            )
            .to_string(),
            "build failed: undefined reference"
        );
    }

    #[test]
    fn zero_and_maximum_capacities_do_not_overflow_or_divide_by_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let map = tmp.path().join("firmware.map");
        std::fs::write(
            &map,
            "Memory Configuration\nName Origin Length\nRAM 0 0\nFLASH 0 18446744073709551615\n",
        )
        .unwrap();
        let text = enrich(FbuildError::BuildFailed("region `RAM' overflowed by 1 bytes\nregion `FLASH' overflowed by 18446744073709551615 bytes".into()), Some(&map)).to_string();
        assert!(text.contains("RAM: RAM overflowed by 1 bytes (region 0 bytes)"));
        assert!(text.contains("requires 36893488147419103230 bytes (200.0%)"));
    }
}
