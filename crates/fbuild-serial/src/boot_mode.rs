//! Per-family "device is in a non-firmware boot/wedge state" line classifier.
//!
//! FastLED/fbuild#688 — generalized the original ESP-only ROM-download
//! detection into a polymorphic [`BootModeClassifier`] trait + [`Registry`]
//! keyed by [`crate::boards::BoardFamily`]. The original
//! [`detect_download_mode`] entry point stays as a thin shim over the
//! ESP classifier so existing callers (and the FastLED/fbuild#532
//! S3-boot-mode hardening) keep working unchanged.
//!
//! ## Why this is per-family
//!
//! Every chip family produces a small, recognizable set of "wedged"
//! signatures:
//!
//! - **ESP32**: `"waiting for download"` / `"DOWNLOAD(USB/UART...)"`
//!   (covered today).
//! - **ARM Cortex-M (LPC8xx, STM32, SAMD)**: `[HARDFAULT pc=... lr=...]`
//!   from the firmware's HardFault handler (`zackees/ArduinoCore-LPC8xx#31`).
//!   The LPC845-BRK bring-up that drove FastLED/FastLED#3300 / #3325 /
//!   #3339 produces exactly this when the bring-up RPC code wedges.
//! - **RP2040**: BOOTSEL drops to USB MSC — not a serial signature,
//!   handled separately (FastLED/fbuild#693).
//! - **AVR**: watchdog reset loops — line-rate-over-time analysis,
//!   not single-line (out of scope here).
//!
//! See FastLED/fbuild#688 for the rationale and roadmap.
//!
//! ## Recovery
//!
//! The matching *recovery* primitive — `DTR/RTS pulse` for ESP, SWD
//! for ARM, 1200-bps touch for RP2040/SAMD/Teensy — lives in
//! [`crate::esp_reset::dispatch_reset`] (FastLED/fbuild#687). Callers
//! that detect a wedge should follow up with `dispatch_reset` against
//! the same family.

use crate::boards::BoardFamily;

/// A detected "device is in a non-firmware boot/wedge state" signal.
///
/// FastLED/fbuild#688. New variants are non-breaking adds — the enum
/// is intentionally exhaustive so a match in the call sites that
/// surface signals fails to compile when a new wedge class lands and
/// the call site has to make a deliberate choice about how to
/// surface it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootModeSignal {
    /// ESP ROM is idle, waiting for a download over USB/UART
    /// (`waiting for download`).
    WaitingForDownload,
    /// ESP boot straps selected ROM download mode
    /// (`boot:0x.. DOWNLOAD(USB/UART...)`).
    DownloadModeSelected,
    /// ARM Cortex-M HardFault handler emitted a crash line —
    /// `[HARDFAULT pc=0xNNNNNNNN lr=0xNNNNNNNN]` per
    /// `zackees/ArduinoCore-LPC8xx#31`. The `pc` / `lr` values are
    /// parsed when present; absent values are `None` so callers can
    /// still surface "we saw a HardFault, registers not parseable"
    /// without losing the signal.
    ArmHardFault { pc: Option<u32>, lr: Option<u32> },
}

impl BootModeSignal {
    /// A human-readable, actionable diagnostic for this signal.
    #[must_use]
    pub fn diagnostic(&self) -> String {
        match self {
            BootModeSignal::WaitingForDownload => {
                "ESP chip is in ROM download mode (\"waiting for download\") — it is \
                 not running application firmware. Power-cycle the board or issue a \
                 DTR/RTS reset to return to run mode."
                    .to_string()
            }
            BootModeSignal::DownloadModeSelected => {
                "ESP boot straps selected ROM download mode (DOWNLOAD(USB/UART)) — the \
                 board will not run firmware until reset. Power-cycle or DTR/RTS-reset \
                 to recover."
                    .to_string()
            }
            BootModeSignal::ArmHardFault { pc, lr } => {
                let pc_str = pc
                    .map(|v| format!("0x{v:08X}"))
                    .unwrap_or_else(|| "(unparsed)".to_string());
                let lr_str = lr
                    .map(|v| format!("0x{v:08X}"))
                    .unwrap_or_else(|| "(unparsed)".to_string());
                format!(
                    "ARM Cortex-M HardFault detected (pc={pc_str}, lr={lr_str}). \
                     The MCU crashed in firmware — issue an SWD reset (e.g. pyOCD/\
                     probe-rs through `fbuild deploy`, see fbuild#687) or power-cycle \
                     the board. Cross-check `lr` against the linker map to find the \
                     calling function."
                )
            }
        }
    }
}

/// Polymorphic line-classifier for a board family.
///
/// FastLED/fbuild#688. One implementation per family per signature
/// pattern. The [`Registry`] holds the right set for a
/// [`BoardFamily`].
pub trait BootModeClassifier: Send + Sync {
    /// Inspect a single serial line. Return a typed signal if the
    /// line indicates the device is in a non-firmware boot/wedge
    /// state; `None` otherwise.
    fn classify(&self, line: &str) -> Option<BootModeSignal>;

    /// Stable name for log/error reporting
    /// ("esp_rom" / "arm_hardfault" / …).
    fn name(&self) -> &'static str;
}

/// ESP ROM `waiting for download` / `DOWNLOAD(USB/UART)` matcher.
/// Mirrors the original [`detect_download_mode`] behavior exactly.
pub struct EspRomDownloadClassifier;

impl BootModeClassifier for EspRomDownloadClassifier {
    fn classify(&self, line: &str) -> Option<BootModeSignal> {
        let lower = line.to_ascii_lowercase();
        if lower.contains("waiting for download") {
            return Some(BootModeSignal::WaitingForDownload);
        }
        if lower.contains("download(usb/uart") || lower.contains("download(uart") {
            return Some(BootModeSignal::DownloadModeSelected);
        }
        None
    }

    fn name(&self) -> &'static str {
        "esp_rom"
    }
}

/// ARM Cortex-M HardFault handler crash-line matcher.
///
/// Pattern: `[HARDFAULT pc=0xNNNNNNNN lr=0xNNNNNNNN]`, matching the
/// `zackees/ArduinoCore-LPC8xx#31` HardFault handler that the
/// LPC845-BRK bring-up firmware uses. The `pc` and `lr` register
/// values are parsed when present (as `u32`); a malformed value or
/// missing field leaves the option `None` so the signal still fires
/// with whatever was parseable.
///
/// Case-insensitive and substring-based so a monitor-prepended
/// timestamp doesn't drop the match.
pub struct ArmHardFaultClassifier;

impl BootModeClassifier for ArmHardFaultClassifier {
    fn classify(&self, line: &str) -> Option<BootModeSignal> {
        let upper = line.to_ascii_uppercase();
        if !upper.contains("HARDFAULT") {
            return None;
        }
        let pc = extract_hex32(line, "pc=");
        let lr = extract_hex32(line, "lr=");
        Some(BootModeSignal::ArmHardFault { pc, lr })
    }

    fn name(&self) -> &'static str {
        "arm_hardfault"
    }
}

fn extract_hex32(line: &str, prefix: &str) -> Option<u32> {
    // Case-insensitive prefix search on the lowercased copy, then
    // index back into the original to grab the hex digits.
    let lower = line.to_ascii_lowercase();
    let idx = lower.find(prefix)?;
    let rest = &line[idx + prefix.len()..];
    let rest = rest.trim_start();
    let hex = rest
        .strip_prefix("0x")
        .or_else(|| rest.strip_prefix("0X"))
        .unwrap_or(rest);
    let digits: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if digits.is_empty() {
        return None;
    }
    u32::from_str_radix(&digits, 16).ok()
}

/// A registry of [`BootModeClassifier`]s for a board family.
///
/// FastLED/fbuild#688. Built via [`Self::for_family`]; consult via
/// [`Self::classify`] for each incoming serial line — the first
/// classifier to return `Some` wins (registry order is meaningful).
pub struct Registry {
    classifiers: Vec<Box<dyn BootModeClassifier>>,
}

impl Registry {
    /// Build the canonical registry for a [`BoardFamily`].
    ///
    /// - ESP families → `EspRomDownloadClassifier`.
    /// - `CdcAcmBridge` (LPC8xx via LPC11U35) → `ArmHardFaultClassifier`.
    /// - Teensy / SAMD / RP2040 (`NativeUsbCdcReset1200Bps`) →
    ///   `ArmHardFaultClassifier` (covers the SAMD51 case; RP2040
    ///   BOOTSEL detection is the scope of FastLED/fbuild#693, USB-
    ///   level not serial-line).
    /// - `ArduinoAutoReset` → no classifier today (AVR watchdog loop
    ///   detection is single-line-insufficient, see #688 out-of-scope).
    #[must_use]
    pub fn for_family(family: BoardFamily) -> Self {
        use BoardFamily::*;
        let classifiers: Vec<Box<dyn BootModeClassifier>> = match family {
            Esp32NativeUsbCdc | Esp32ExternalUart => {
                vec![Box::new(EspRomDownloadClassifier)]
            }
            CdcAcmBridge | Teensy | NativeUsbCdcReset1200Bps => {
                vec![Box::new(ArmHardFaultClassifier)]
            }
            ArduinoAutoReset => vec![],
        };
        Self { classifiers }
    }

    /// Classify a single line. Returns the FIRST matching
    /// classifier's `(name, signal)` tuple; later classifiers are
    /// short-circuited.
    #[must_use]
    pub fn classify(&self, line: &str) -> Option<(&'static str, BootModeSignal)> {
        for c in &self.classifiers {
            if let Some(sig) = c.classify(line) {
                return Some((c.name(), sig));
            }
        }
        None
    }

    /// Number of classifiers registered (test seam / observability).
    #[must_use]
    pub fn len(&self) -> usize {
        self.classifiers.len()
    }

    /// Whether the registry is empty (e.g. `ArduinoAutoReset` today).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.classifiers.is_empty()
    }
}

// ─── Legacy entry point — thin shim over EspRomDownloadClassifier ───

/// Inspect one serial line for an ESP ROM download-mode indicator.
///
/// **Backwards-compat shim** (FastLED/fbuild#688) — exists so the
/// FastLED/fbuild#532 S3-boot-mode-hardening path doesn't need a
/// caller update. New callers should construct a
/// [`Registry::for_family`] and call [`Registry::classify`] instead.
///
/// Matching is case-insensitive and substring-based so it survives
/// the leading `boot:0xNN ` strap prefix and any timestamp the
/// monitor prepends.
pub fn detect_download_mode(line: &str) -> Option<BootModeSignal> {
    EspRomDownloadClassifier.classify(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Backwards-compat (existing #532 path) ──────────────────────

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
        assert!(
            !BootModeSignal::ArmHardFault {
                pc: Some(0x100),
                lr: Some(0x200),
            }
            .diagnostic()
            .is_empty()
        );
    }

    // ─── FastLED/fbuild#688: ArmHardFault matcher ───────────────────

    #[test]
    fn arm_hardfault_detected_with_canonical_format() {
        let line = "[HARDFAULT pc=0x00000C5A lr=0x00001234]";
        assert_eq!(
            ArmHardFaultClassifier.classify(line),
            Some(BootModeSignal::ArmHardFault {
                pc: Some(0x0000_0C5A),
                lr: Some(0x0000_1234),
            })
        );
    }

    #[test]
    fn arm_hardfault_detected_case_insensitive() {
        let line = "[hardfault pc=0xABCDEF01 lr=0x00000010]";
        assert_eq!(
            ArmHardFaultClassifier.classify(line),
            Some(BootModeSignal::ArmHardFault {
                pc: Some(0xABCD_EF01),
                lr: Some(0x0000_0010),
            })
        );
    }

    #[test]
    fn arm_hardfault_with_timestamp_prefix() {
        let line = "00:03.21 [HARDFAULT pc=0x000019AA lr=0x00002000]";
        let sig = ArmHardFaultClassifier.classify(line).unwrap();
        match sig {
            BootModeSignal::ArmHardFault { pc, lr } => {
                assert_eq!(pc, Some(0x0000_19AA));
                assert_eq!(lr, Some(0x0000_2000));
            }
            other => panic!("expected ArmHardFault, got {other:?}"),
        }
    }

    #[test]
    fn arm_hardfault_without_register_values_still_fires() {
        // Some boards' HardFault handlers don't print pc/lr — surface
        // the signal anyway with None/None.
        let line = "[HARDFAULT]";
        assert_eq!(
            ArmHardFaultClassifier.classify(line),
            Some(BootModeSignal::ArmHardFault { pc: None, lr: None })
        );
    }

    #[test]
    fn arm_hardfault_ignores_unrelated_lines() {
        assert!(
            ArmHardFaultClassifier
                .classify("Hello from app_main")
                .is_none()
        );
        assert!(
            ArmHardFaultClassifier
                .classify("boot:0x13 SPI_FAST_FLASH_BOOT")
                .is_none()
        );
        assert!(ArmHardFaultClassifier.classify("").is_none());
    }

    // ─── FastLED/fbuild#688: Registry per-family wiring ─────────────

    #[test]
    fn esp_families_registry_picks_esp_classifier() {
        let r = Registry::for_family(BoardFamily::Esp32NativeUsbCdc);
        assert_eq!(r.len(), 1);
        let (name, sig) = r.classify("waiting for download").unwrap();
        assert_eq!(name, "esp_rom");
        assert_eq!(sig, BootModeSignal::WaitingForDownload);
    }

    #[test]
    fn cdc_bridge_registry_picks_arm_hardfault_classifier() {
        let r = Registry::for_family(BoardFamily::CdcAcmBridge);
        assert_eq!(r.len(), 1);
        let (name, sig) = r
            .classify("[HARDFAULT pc=0x000019AA lr=0x0000040E]")
            .unwrap();
        assert_eq!(name, "arm_hardfault");
        assert_eq!(
            sig,
            BootModeSignal::ArmHardFault {
                pc: Some(0x0000_19AA),
                lr: Some(0x0000_040E),
            }
        );
    }

    #[test]
    fn arduino_registry_is_empty() {
        // AVR watchdog-loop detection is line-rate-over-time, not
        // single-line — out of scope for this issue (#688).
        let r = Registry::for_family(BoardFamily::ArduinoAutoReset);
        assert!(r.is_empty());
        assert!(r.classify("[HARDFAULT pc=0x0 lr=0x0]").is_none());
    }

    #[test]
    fn registry_returns_none_for_unrelated_lines() {
        let r = Registry::for_family(BoardFamily::Esp32NativeUsbCdc);
        assert!(r.classify("Hello from app_main").is_none());
        assert!(r.classify("").is_none());
    }

    #[test]
    fn arm_registry_does_not_fire_on_esp_signals() {
        // Cross-check: the ARM registry MUST NOT match ESP signatures.
        // (Today's ARM registry has only the HardFault matcher, so
        // this test pins that no future addition reclassifies ESP
        // signals under the ARM family.)
        let r = Registry::for_family(BoardFamily::CdcAcmBridge);
        assert!(r.classify("waiting for download").is_none());
        assert!(r.classify("boot:0x23 DOWNLOAD(USB/UART0)").is_none());
    }
}
