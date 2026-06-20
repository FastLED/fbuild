//! ESP ROM boot-mode detection.
//!
//! After a flash / reset / monitor handoff, an ESP chip can be left in the
//! serial ROM bootloader ("download mode") instead of running application
//! firmware. On the wire this shows up as a fixed set of ROM strings, e.g.
//! `waiting for download` and `boot:0x23 DOWNLOAD(USB/UART0)`.
//!
//! Surfacing a targeted diagnostic when those appear — rather than letting the
//! monitor sit silently while the board does nothing — lets callers (and
//! humans reading the logs) recognise the boot-mode lockup and recover, rather
//! than mistaking a stuck-in-ROM board for a host-side deadlock. See
//! FastLED/fbuild#532.
//!
//! The matching *recovery* primitive — a single DTR/RTS-driven hard-reset
//! sequence — lives in [`crate::esp_reset`]. Callers that want to attempt
//! automatic recovery before propagating the error can invoke
//! [`crate::esp_reset::esp_hard_reset_blocking`] on the same port that produced
//! the detection.

/// A detected ESP ROM download-mode signal on the serial stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootModeSignal {
    /// ROM is idle, waiting for a download over USB/UART
    /// (`waiting for download`).
    WaitingForDownload,
    /// Boot straps selected ROM download mode
    /// (`boot:0x.. DOWNLOAD(USB/UART...)`).
    DownloadModeSelected,
}

impl BootModeSignal {
    /// A human-readable, actionable diagnostic for this signal.
    pub fn diagnostic(self) -> &'static str {
        match self {
            BootModeSignal::WaitingForDownload => {
                "ESP chip is in ROM download mode (\"waiting for download\") — it is \
                 not running application firmware. Power-cycle the board or issue a \
                 DTR/RTS reset to return to run mode."
            }
            BootModeSignal::DownloadModeSelected => {
                "ESP boot straps selected ROM download mode (DOWNLOAD(USB/UART)) — the \
                 board will not run firmware until reset. Power-cycle or DTR/RTS-reset \
                 to recover."
            }
        }
    }
}

/// Inspect one serial line for an ESP ROM download-mode indicator.
///
/// Matching is case-insensitive and substring-based so it survives the leading
/// `boot:0xNN ` strap prefix and any timestamp the monitor prepends.
pub fn detect_download_mode(line: &str) -> Option<BootModeSignal> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("waiting for download") {
        return Some(BootModeSignal::WaitingForDownload);
    }
    if lower.contains("download(usb/uart") || lower.contains("download(uart") {
        return Some(BootModeSignal::DownloadModeSelected);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_for_download_detected() {
        assert_eq!(
            detect_download_mode("waiting for download"),
            Some(BootModeSignal::WaitingForDownload)
        );
    }

    #[test]
    fn download_strap_line_detected() {
        assert_eq!(
            detect_download_mode("boot:0x23 (DOWNLOAD(USB/UART0))"),
            Some(BootModeSignal::DownloadModeSelected)
        );
        assert_eq!(
            detect_download_mode("rst:0x1 (POWERON),boot:0x23 DOWNLOAD(UART0)"),
            Some(BootModeSignal::DownloadModeSelected)
        );
    }

    #[test]
    fn case_insensitive_and_timestamp_prefixed() {
        assert_eq!(
            detect_download_mode("00:03.21 WAITING FOR DOWNLOAD"),
            Some(BootModeSignal::WaitingForDownload)
        );
    }

    #[test]
    fn normal_boot_and_app_lines_ignored() {
        assert_eq!(
            detect_download_mode("boot:0x13 (SPI_FAST_FLASH_BOOT)"),
            None
        );
        assert_eq!(detect_download_mode("Hello from app_main"), None);
        assert_eq!(detect_download_mode(""), None);
    }

    #[test]
    fn diagnostics_are_non_empty() {
        assert!(!BootModeSignal::WaitingForDownload.diagnostic().is_empty());
        assert!(!BootModeSignal::DownloadModeSelected.diagnostic().is_empty());
    }
}
