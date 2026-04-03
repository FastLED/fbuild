//! Shared build output formatting for all platform orchestrators.
//!
//! Provides uniform build logging across AVR, ESP32, and Teensy platforms.
//! Each function writes formatted lines to a [`BuildLog`], which optionally
//! streams them in real-time through a channel sender.

use std::path::Path;

use fbuild_core::{BuildLog, SizeInfo};

/// Create a [`BuildLog`], optionally wired to a real-time streaming sender.
pub fn create_build_log(sender: Option<std::sync::mpsc::Sender<String>>) -> BuildLog {
    match sender {
        Some(s) => BuildLog::with_sender(s),
        None => BuildLog::new(),
    }
}

/// Emit the `BUILDING <env_name>` banner.
///
/// ```text
/// ============================
/// =   BUILDING env_name   =
/// ============================
/// ```
pub fn log_build_banner(log: &mut BuildLog, env_name: &str) {
    let banner = format!("=   BUILDING {}   =", env_name);
    let border = "=".repeat(banner.len());
    log.push(border.clone());
    log.push(banner);
    log.push(border);
}

/// Emit board/MCU/frequency and memory limits.
///
/// `f_cpu` is the raw string from board config (e.g. `"16000000L"`).
/// Parses it, strips the trailing `L`, and converts to MHz.
pub fn log_board_info(
    log: &mut BuildLog,
    board_name: &str,
    mcu: &str,
    f_cpu: &str,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
) {
    let f_cpu_mhz = f_cpu.trim_end_matches('L').parse::<u64>().unwrap_or(0) / 1_000_000;
    log.push(format!(
        "Board: {} / {} @ {}MHz",
        board_name,
        mcu.to_uppercase(),
        f_cpu_mhz
    ));
    if let (Some(flash), Some(ram)) = (max_flash, max_ram) {
        log.push(format!(
            "Memory: {} Flash, {} RAM",
            format_bytes(flash),
            format_bytes(ram)
        ));
    }
}

/// Emit toolchain version line (e.g. `Toolchain: avr-gcc 7.3.0`).
pub fn log_toolchain_version(log: &mut BuildLog, label: &str, version: &str) {
    log.push(format!("Toolchain: {} {}", label, version));
}

/// Log a single file being compiled (for sequential compilation).
pub fn log_compiling(log: &mut BuildLog, object_path: &Path) {
    log.push(format!("Compiling {}", object_path.display()));
}

/// Log a link/convert step (e.g. `"Linking firmware.elf"`, `"Building firmware.hex"`).
pub fn log_linking(log: &mut BuildLog, step_name: &str) {
    log.push(step_name.to_string());
}

/// Collect non-empty compiler warnings (stderr lines) into the build log.
pub fn collect_warnings(stderr: &str, log: &mut BuildLog) {
    let trimmed = stderr.trim();
    if !trimmed.is_empty() {
        for line in trimmed.lines() {
            log.push(line.to_string());
        }
    }
}

/// Emit size reporting with totals and percentages.
///
/// ```text
/// Flash: 5.14KB / 31.50KB (16.3%)
/// RAM:   597 bytes / 2.00KB (29.2%)
/// ```
pub fn log_size_report(log: &mut BuildLog, size: &SizeInfo) {
    log.push(format!(
        "Flash: {} / {} ({:.1}%)",
        format_bytes(size.total_flash),
        format_bytes(size.max_flash.unwrap_or(0)),
        size.flash_percent().unwrap_or(0.0),
    ));
    log.push(format!(
        "RAM:   {} / {} ({:.1}%)",
        format_bytes(size.total_ram),
        format_bytes(size.max_ram.unwrap_or(0)),
        size.ram_percent().unwrap_or(0.0),
    ));
}

/// Emit an artifact listing line with its on-disk size.
///
/// Output: `Artifact: /path/to/firmware.elf (15.20KB)`
pub fn log_artifact(log: &mut BuildLog, path: &Path) {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    log.push(format!(
        "Artifact: {} ({})",
        path.display(),
        format_bytes(size)
    ));
}

/// Format a byte count as a human-readable string.
///
/// Examples: `"31.50KB"`, `"2.00MB"`, `"512 bytes"`
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.2}MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.2}KB", bytes as f64 / 1024.0)
    } else {
        format!("{} bytes", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes_bytes() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1023), "1023 bytes");
    }

    #[test]
    fn test_format_bytes_kb() {
        assert_eq!(format_bytes(1024), "1.00KB");
        assert_eq!(format_bytes(32256), "31.50KB");
    }

    #[test]
    fn test_format_bytes_mb() {
        assert_eq!(format_bytes(1024 * 1024), "1.00MB");
        assert_eq!(format_bytes(2 * 1024 * 1024 + 512 * 1024), "2.50MB");
    }

    #[test]
    fn test_log_build_banner() {
        let mut log = BuildLog::new();
        log_build_banner(&mut log, "uno");
        let lines = log.into_lines();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], lines[2]); // borders match
        assert!(lines[1].contains("BUILDING uno"));
    }

    #[test]
    fn test_log_board_info() {
        let mut log = BuildLog::new();
        log_board_info(
            &mut log,
            "Arduino Uno",
            "atmega328p",
            "16000000L",
            Some(32256),
            Some(2048),
        );
        let lines = log.into_lines();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("Arduino Uno / ATMEGA328P @ 16MHz"));
        assert!(lines[1].contains("31.50KB Flash"));
        assert!(lines[1].contains("2.00KB RAM"));
    }

    #[test]
    fn test_log_board_info_no_max() {
        let mut log = BuildLog::new();
        log_board_info(&mut log, "Board", "mcu", "8000000", None, None);
        let lines = log.into_lines();
        assert_eq!(lines.len(), 1); // no memory line
        assert!(lines[0].contains("@ 8MHz"));
    }

    #[test]
    fn test_log_toolchain_version() {
        let mut log = BuildLog::new();
        log_toolchain_version(&mut log, "avr-gcc", "7.3.0");
        let lines = log.into_lines();
        assert_eq!(lines, vec!["Toolchain: avr-gcc 7.3.0"]);
    }

    #[test]
    fn test_collect_warnings_empty() {
        let mut log = BuildLog::new();
        collect_warnings("", &mut log);
        collect_warnings("   \n  ", &mut log);
        assert!(log.is_empty());
    }

    #[test]
    fn test_collect_warnings() {
        let mut log = BuildLog::new();
        collect_warnings("warning: unused variable\nnote: see here", &mut log);
        let lines = log.into_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "warning: unused variable");
    }

    #[test]
    fn test_log_size_report() {
        let mut log = BuildLog::new();
        let size = SizeInfo {
            text: 5000,
            data: 200,
            bss: 400,
            total_flash: 5200,
            total_ram: 600,
            max_flash: Some(32256),
            max_ram: Some(2048),
        };
        log_size_report(&mut log, &size);
        let lines = log.into_lines();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("Flash:"));
        assert!(lines[0].contains("5.08KB"));
        assert!(lines[0].contains("31.50KB"));
        assert!(lines[1].starts_with("RAM:"));
    }

    #[test]
    fn test_log_size_report_no_max() {
        let mut log = BuildLog::new();
        let size = SizeInfo {
            text: 1000,
            data: 100,
            bss: 200,
            total_flash: 1100,
            total_ram: 300,
            max_flash: None,
            max_ram: None,
        };
        log_size_report(&mut log, &size);
        let lines = log.into_lines();
        assert!(lines[0].contains("0.0%"));
    }

    #[test]
    fn test_create_build_log_no_sender() {
        let log = create_build_log(None);
        assert!(log.is_empty());
    }

    #[test]
    fn test_create_build_log_with_sender() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut log = create_build_log(Some(tx));
        log.push("test");
        assert_eq!(rx.try_recv().unwrap(), "test");
    }
}
