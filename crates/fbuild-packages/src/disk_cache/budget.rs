//! Size budgets and auto-scaling for the disk cache GC.
//!
//! Budgets auto-scale at startup based on available disk space,
//! capped at absolute maximums. All values are overridable via environment variables.

use std::path::Path;

/// Default absolute caps.
const DEFAULT_ARCHIVE_BUDGET: u64 = 15 * 1024 * 1024 * 1024; // 15 GiB
const DEFAULT_INSTALLED_BUDGET: u64 = 15 * 1024 * 1024 * 1024; // 15 GiB
const DEFAULT_HIGH_WATERMARK: u64 = 30 * 1024 * 1024 * 1024; // 30 GiB

/// Percentage of total disk for per-phase budgets.
const PHASE_DISK_PERCENT: f64 = 0.05; // 5%

/// Percentage of total disk for combined high watermark.
const COMBINED_DISK_PERCENT: f64 = 0.10; // 10%

/// Low watermark is 80% of high watermark — GC stops here.
const LOW_WATERMARK_RATIO: f64 = 0.80;

/// Computed cache budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheBudget {
    pub archive_budget: u64,
    pub installed_budget: u64,
    pub high_watermark: u64,
    pub low_watermark: u64,
}

impl CacheBudget {
    /// Compute budgets based on disk size, respecting env overrides.
    pub fn compute(cache_root: &Path) -> Self {
        let total_disk = get_total_disk_space(cache_root);
        Self::compute_with_disk_size(total_disk)
    }

    /// Compute budgets with a known disk size (for testing).
    pub fn compute_with_disk_size(total_disk: u64) -> Self {
        let phase_by_disk = (total_disk as f64 * PHASE_DISK_PERCENT) as u64;
        let combined_by_disk = (total_disk as f64 * COMBINED_DISK_PERCENT) as u64;

        let archive_budget = parse_env_size("FBUILD_CACHE_ARCHIVE_BUDGET")
            .unwrap_or_else(|| DEFAULT_ARCHIVE_BUDGET.min(phase_by_disk));

        let installed_budget = parse_env_size("FBUILD_CACHE_INSTALLED_BUDGET")
            .unwrap_or_else(|| DEFAULT_INSTALLED_BUDGET.min(phase_by_disk));

        let high_watermark = parse_env_size("FBUILD_CACHE_HIGH_WATERMARK")
            .unwrap_or_else(|| DEFAULT_HIGH_WATERMARK.min(combined_by_disk));

        let low_watermark = (high_watermark as f64 * LOW_WATERMARK_RATIO) as u64;

        Self {
            archive_budget,
            installed_budget,
            high_watermark,
            low_watermark,
        }
    }
}

/// Parse a human-readable size from an environment variable.
/// Supports suffixes: B, K, KB, M, MB, G, GB, T, TB (case-insensitive).
fn parse_env_size(var: &str) -> Option<u64> {
    let val = std::env::var(var).ok()?;
    parse_human_size(&val)
}

/// Parse a human-readable size string like "15G", "500M", "1024".
pub fn parse_human_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Find where digits end and suffix begins
    let (num_part, suffix) = {
        let idx = s
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len());
        (&s[..idx], s[idx..].trim().to_uppercase())
    };

    let num: f64 = num_part.parse().ok()?;
    if num.is_nan() || num.is_infinite() || num < 0.0 {
        return None;
    }

    let multiplier: u64 = match suffix.as_str() {
        "" | "B" => 1,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
        "T" | "TB" | "TIB" => 1024 * 1024 * 1024 * 1024,
        _ => return None,
    };

    Some((num * multiplier as f64) as u64)
}

/// Get total disk space for the filesystem containing the given path.
fn get_total_disk_space(path: &Path) -> u64 {
    #[cfg(windows)]
    {
        get_total_disk_space_windows(path)
    }
    #[cfg(unix)]
    {
        get_total_disk_space_unix(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        // Fallback: assume 500 GB
        500 * 1024 * 1024 * 1024
    }
}

#[cfg(windows)]
fn get_total_disk_space_windows(path: &Path) -> u64 {
    // Use PowerShell to get disk info. Route through fbuild-core's
    // `run_command` so the probe spawn is captured by the daemon's
    // containment group (issue #32) — a daemon crash mid-GC must not
    // leak a `powershell.exe` or its `conhost.exe` wrapper.
    let path_str = path.to_string_lossy();
    let drive = if path_str.len() >= 2 && path_str.as_bytes()[1] == b':' {
        &path_str[..2]
    } else {
        "C:"
    };
    let ps_cmd = format!(
        "(Get-PSDrive -Name '{}').Used + (Get-PSDrive -Name '{}').Free",
        &drive[..1],
        &drive[..1]
    );
    fbuild_core::subprocess::run_command(
        &["powershell", "-NoProfile", "-Command", &ps_cmd],
        None,
        None,
        None,
    )
    .ok()
    .and_then(|o| o.stdout.trim().parse::<u64>().ok())
    .unwrap_or(500 * 1024 * 1024 * 1024) // fallback 500 GB
}

#[cfg(unix)]
fn get_total_disk_space_unix(path: &Path) -> u64 {
    // Use `df -Pk` for portable 1024-byte block output.
    // `-P` gives POSIX format, `-k` forces 1024-byte blocks on all
    // platforms. Route through the containment group (issue #32).
    let path_str = path.to_string_lossy();
    let output =
        fbuild_core::subprocess::run_command(&["df", "-P", "-k", &path_str], None, None, None);

    output
        .ok()
        .and_then(|o| {
            o.stdout
                .lines()
                .nth(1) // skip header
                .and_then(|line| {
                    // POSIX df -Pk columns: Filesystem 1024-blocks Used Available Capacity Mounted
                    line.split_whitespace()
                        .nth(1) // total 1024-byte blocks
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(|blocks| blocks * 1024)
                })
        })
        .unwrap_or(500 * 1024 * 1024 * 1024) // fallback 500 GB
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_human_size_bytes() {
        assert_eq!(parse_human_size("1024"), Some(1024));
        assert_eq!(parse_human_size("1024B"), Some(1024));
    }

    #[test]
    fn test_parse_human_size_kilobytes() {
        assert_eq!(parse_human_size("1K"), Some(1024));
        assert_eq!(parse_human_size("1KB"), Some(1024));
        assert_eq!(parse_human_size("2k"), Some(2048));
    }

    #[test]
    fn test_parse_human_size_megabytes() {
        assert_eq!(parse_human_size("1M"), Some(1024 * 1024));
        assert_eq!(parse_human_size("500M"), Some(500 * 1024 * 1024));
        assert_eq!(parse_human_size("1mb"), Some(1024 * 1024));
    }

    #[test]
    fn test_parse_human_size_gigabytes() {
        assert_eq!(parse_human_size("1G"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_human_size("15G"), Some(15 * 1024 * 1024 * 1024));
        assert_eq!(parse_human_size("1gb"), Some(1024 * 1024 * 1024));
    }

    #[test]
    fn test_parse_human_size_terabytes() {
        assert_eq!(parse_human_size("1T"), Some(1024 * 1024 * 1024 * 1024));
    }

    #[test]
    fn test_parse_human_size_empty() {
        assert_eq!(parse_human_size(""), None);
        assert_eq!(parse_human_size("  "), None);
    }

    #[test]
    fn test_parse_human_size_invalid() {
        assert_eq!(parse_human_size("abc"), None);
        assert_eq!(parse_human_size("10X"), None);
        assert_eq!(parse_human_size("-5G"), None);
    }

    #[test]
    fn test_parse_human_size_nan_inf() {
        assert_eq!(parse_human_size("NaN"), None);
        assert_eq!(parse_human_size("inf"), None);
        assert_eq!(parse_human_size("InfG"), None);
    }

    #[test]
    fn test_budget_autoscales_to_disk() {
        // Small disk: 100 GB
        let small_disk = 100 * 1024 * 1024 * 1024_u64;
        let budget = CacheBudget::compute_with_disk_size(small_disk);

        // 5% of 100 GB = 5 GB < 15 GB cap, so should be 5 GB
        assert_eq!(budget.archive_budget, 5 * 1024 * 1024 * 1024);
        assert_eq!(budget.installed_budget, 5 * 1024 * 1024 * 1024);

        // 10% of 100 GB = 10 GB < 30 GB cap
        assert_eq!(budget.high_watermark, 10 * 1024 * 1024 * 1024);

        // Low watermark = 80% of high
        assert_eq!(budget.low_watermark, 8 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_budget_caps_on_large_disk() {
        // Very large disk: 2 TB
        let large_disk = 2 * 1024 * 1024 * 1024 * 1024_u64;
        let budget = CacheBudget::compute_with_disk_size(large_disk);

        // 5% of 2 TB = 102 GB > 15 GB cap, so capped at 15 GB
        assert_eq!(budget.archive_budget, 15 * 1024 * 1024 * 1024);
        assert_eq!(budget.installed_budget, 15 * 1024 * 1024 * 1024);

        // 10% of 2 TB = 204 GB > 30 GB cap
        assert_eq!(budget.high_watermark, 30 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_low_watermark_is_80_percent() {
        let budget = CacheBudget::compute_with_disk_size(200 * 1024 * 1024 * 1024);
        assert_eq!(
            budget.low_watermark,
            (budget.high_watermark as f64 * 0.80) as u64
        );
    }
}
