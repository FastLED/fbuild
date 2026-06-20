//! ESP32 hard-reset DTR/RTS sequence + companion CDC-ACM bridge safety
//! primitive.
//!
//! Most ESP DevKit boards (and the ESP32-S3 USB serial/JTAG bridge) wire DTR
//! and RTS to BOOT (IO0) and EN/RESET via a transistor inverter pair, so the
//! host-side serial line-control bits determine what mode the chip boots
//! into. Without an explicit, well-timed sequence, fbuild can leave the
//! lines asserted in a configuration that puts the chip into ROM download
//! mode — the failure described in FastLED/fbuild#532.
//!
//! Detection of a stuck-in-ROM board already lives in
//! [`crate::boot_mode::detect_download_mode`]. This module supplies the
//! matching *recovery* primitive: a single function that pulses the lines
//! through the canonical hard-reset sequence and brings the chip back to
//! normal firmware boot.
//!
//! Wiring assumed (matches every Espressif DevKit + USB-CDC variant):
//!
//! | DTR | RTS | Effect                            |
//! |-----|-----|-----------------------------------|
//! | low | low | boot from flash (run firmware)    |
//! | low | high| EN low → reset hold               |
//! | high| low | BOOT low → enter ROM bootloader   |
//!
//! Sequence implemented here keeps DTR=low (so BOOT stays high → boot from
//! flash) and pulses RTS to toggle EN, matching `esptool`'s
//! `hard_reset` for classic-hardware UART and USB-CDC bridges.
//!
//! # ⚠️ Do NOT call [`esp_hard_reset_blocking`] for CDC-ACM bridge boards
//!
//! The post-reset idle state this function leaves (DTR=low, RTS=low) is
//! "host not ready" for boards that talk through a USB-VCOM bridge chip —
//! LPC11U35 on LPC845-BRK / LPCXpresso845-MAX / LPC804, FTDI FT232 in CDC
//! mode, some CH340 firmware revisions. The bridge silently drops every
//! byte the target MCU transmits and the device looks dead. FastLED's
//! Python side cost two debugging sessions to this exact failure on the
//! LPC845-BRK; see FastLED/FastLED#3300 and FastLED/FastLED#3339 for the
//! root cause, and FastLED/fbuild#684 for the audit that brought the
//! lesson here.
//!
//! For CDC-ACM bridge boards:
//!
//! - Reset via SWD/CMSIS-DAP (e.g. pyOCD for LPC8xx), not DTR/RTS.
//! - If you must re-use [`esp_hard_reset_blocking`] on a CDC board for
//!   some reason, call [`cdc_vcom_safe_assert`] immediately after to
//!   restore the host-ready idle state.
//! - On a fresh port open for a CDC board, call [`cdc_vcom_safe_assert`]
//!   once after the [`serialport::SerialPort`] is constructed so the
//!   bridge sees DTR=true (host ready) before the target MCU starts
//!   emitting. `manager::open_port` already does this unconditionally
//!   per FastLED/fbuild#532 — safe for ESP (the lines are pulsed below
//!   in [`esp_hard_reset_blocking`] anyway) and required for CDC bridges.

use std::thread;
use std::time::Duration;

/// DTR/RTS control surface — exactly what [`esp_hard_reset_blocking`]
/// needs from a serial port. A blanket impl makes every type that
/// implements [`serialport::SerialPort`] usable, and a tiny mock impl
/// makes the sequence unit-testable without real hardware.
pub trait DtrRtsControl {
    fn write_data_terminal_ready(&mut self, level: bool) -> serialport::Result<()>;
    fn write_request_to_send(&mut self, level: bool) -> serialport::Result<()>;
}

impl<T: serialport::SerialPort + ?Sized> DtrRtsControl for T {
    fn write_data_terminal_ready(&mut self, level: bool) -> serialport::Result<()> {
        serialport::SerialPort::write_data_terminal_ready(self, level)
    }
    fn write_request_to_send(&mut self, level: bool) -> serialport::Result<()> {
        serialport::SerialPort::write_request_to_send(self, level)
    }
}

/// Hold time for the RTS=high (EN=low) pulse, in milliseconds.
///
/// Matches `esptool`'s classic-hardware `hard_reset` timing: long enough to
/// let the EN debounce capacitor on a DevKit settle across the chips we've
/// observed, short enough that recovery feels instant to a host caller.
pub const HARD_RESET_PULSE_MS: u64 = 100;

/// Drive the DTR/RTS sequence that takes an ESP out of ROM download mode
/// and into normal firmware boot.
///
/// **For ESP boards only.** See the module-level warning — calling this on
/// a CDC-ACM bridge board (LPC11U35, FTDI CDC, etc.) leaves DTR=low and
/// the bridge silently drops every byte the target transmits.
///
/// Blocking — intended to run inside `tokio::task::spawn_blocking`, matching
/// the rest of `fbuild-serial`'s pattern (see
/// [`crate::manager`] for the precedent: every `serialport` mutation
/// is treated as a synchronous Win32/POSIX call and shipped to the blocking
/// pool to keep the tokio reactor free).
///
/// Sequence (each step is logged at `tracing::debug!`; the completion is
/// logged at `tracing::info!` so a routine log scan can see the recovery
/// happened):
///
/// 1. `DTR = low` — BOOT pin high → chip will boot from flash, not ROM.
/// 2. `RTS = high` — EN pin low → reset hold.
/// 3. Sleep [`HARD_RESET_PULSE_MS`] ms.
/// 4. `RTS = low` — EN pin high → release reset → chip boots from flash.
///
/// Errors from the underlying DTR/RTS writes propagate; the most common
/// cause is the port being closed mid-call. A `serialport::Error` from
/// step 1 short-circuits before any pin is pulsed.
///
/// # Naming
///
/// Renamed from `hard_reset_blocking` (FastLED/fbuild#684) — the previous
/// name implied generality but the function is specifically the ESP
/// classic-reset sequence. The `esp_` prefix makes the family-scope
/// explicit so a future contributor adding an LPC- or FTDI-CDC-board
/// flow does not accidentally call it.
pub fn esp_hard_reset_blocking<P: DtrRtsControl + ?Sized>(port: &mut P) -> serialport::Result<()> {
    tracing::debug!("esp_reset: DTR=low (BOOT high → boot from flash)");
    port.write_data_terminal_ready(false)?;
    tracing::debug!("esp_reset: RTS=high (EN low → reset hold)");
    port.write_request_to_send(true)?;
    thread::sleep(Duration::from_millis(HARD_RESET_PULSE_MS));
    tracing::debug!("esp_reset: RTS=low (EN high → release reset)");
    port.write_request_to_send(false)?;
    tracing::info!(
        "esp_reset: ESP hard-reset complete (DTR=low, RTS pulsed {}ms)",
        HARD_RESET_PULSE_MS
    );
    Ok(())
}

/// Assert the universal "host attached, please forward bytes" idle state
/// on a CDC-ACM USB-VCOM bridge — DTR=true, RTS=true.
///
/// Required for: LPC11U35 VCOM bridge on LPC845-BRK / LPCXpresso845-MAX /
/// LPC804, FTDI FT232 in CDC mode, some CH340 firmware revisions, and any
/// board whose USB endpoint is a bridge chip that treats DTR as a host-
/// ready signal. Without this assert, the bridge silently drops every byte
/// the target MCU transmits and the device looks dead.
///
/// Safe to call on ESP boards too — DTR=true / RTS=true is not a reset
/// trigger (the reset sequence is the *pulse* on RTS implemented in
/// [`esp_hard_reset_blocking`]). It does, however, set BOOT=low (DTR=true
/// → BOOT pin low via the inverter pair), which is the "enter ROM
/// bootloader" state on an ESP if the chip is then reset externally. So
/// don't call this on an ESP path that hasn't already completed boot
/// from flash. The actual production caller — `manager::open_port` —
/// asserts these lines unconditionally on every port open per
/// FastLED/fbuild#532; the chip is already running firmware by then.
///
/// Blocking — same calling convention as [`esp_hard_reset_blocking`].
///
/// # References
///
/// - FastLED/FastLED#3300 — root: LPC845-BRK "silence" that turned out
///   to be host-side DTR misconfig.
/// - FastLED/FastLED#3339 — the Python-side fix this primitive mirrors.
/// - FastLED/fbuild#684 — the audit that brought the lesson here.
pub fn cdc_vcom_safe_assert<P: DtrRtsControl + ?Sized>(port: &mut P) -> serialport::Result<()> {
    tracing::debug!("cdc_vcom_safe_assert: DTR=true (host ready for bridge)");
    port.write_data_terminal_ready(true)?;
    tracing::debug!("cdc_vcom_safe_assert: RTS=true (host ready for bridge)");
    port.write_request_to_send(true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct RecordedPort {
        events: Vec<(&'static str, bool)>,
        fail_on: Option<&'static str>,
    }

    impl DtrRtsControl for RecordedPort {
        fn write_data_terminal_ready(&mut self, level: bool) -> serialport::Result<()> {
            if self.fail_on == Some("dtr") {
                return Err(serialport::Error::new(
                    serialport::ErrorKind::Io(std::io::ErrorKind::Other),
                    "fake DTR failure",
                ));
            }
            self.events.push(("DTR", level));
            Ok(())
        }
        fn write_request_to_send(&mut self, level: bool) -> serialport::Result<()> {
            if self.fail_on == Some("rts") {
                return Err(serialport::Error::new(
                    serialport::ErrorKind::Io(std::io::ErrorKind::Other),
                    "fake RTS failure",
                ));
            }
            self.events.push(("RTS", level));
            Ok(())
        }
    }

    #[test]
    fn esp_hard_reset_emits_canonical_sequence() {
        let mut port = RecordedPort::default();
        esp_hard_reset_blocking(&mut port).expect("esp_hard_reset should succeed against the mock");
        assert_eq!(
            port.events,
            vec![
                ("DTR", false), // BOOT high — boot from flash
                ("RTS", true),  // EN low — reset hold
                ("RTS", false), // EN high — release reset
            ]
        );
    }

    #[test]
    fn pulse_holds_for_at_least_the_minimum_duration() {
        let mut port = RecordedPort::default();
        let start = std::time::Instant::now();
        esp_hard_reset_blocking(&mut port).expect("ok");
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(HARD_RESET_PULSE_MS),
            "reset pulse should hold RTS=high for at least {}ms, only slept {:?}",
            HARD_RESET_PULSE_MS,
            elapsed
        );
    }

    #[test]
    fn dtr_failure_short_circuits_before_any_rts_pulse() {
        let mut port = RecordedPort {
            fail_on: Some("dtr"),
            ..RecordedPort::default()
        };
        assert!(esp_hard_reset_blocking(&mut port).is_err());
        // Sequence step 1 failed; we must NOT have pulsed EN, otherwise we
        // would have left the chip in reset.
        assert!(
            port.events.is_empty(),
            "no RTS transitions should fire after a DTR error, got {:?}",
            port.events
        );
    }

    #[test]
    fn rts_failure_surfaces_to_caller() {
        let mut port = RecordedPort {
            fail_on: Some("rts"),
            ..RecordedPort::default()
        };
        assert!(esp_hard_reset_blocking(&mut port).is_err());
    }

    /// FastLED/fbuild#684 mirror of FastLED/FastLED#3339:
    /// `cdc_vcom_safe_assert` must end with both DTR=true and RTS=true so
    /// CDC-ACM bridges (LPC11U35, FTDI CDC) see "host ready" and forward
    /// the target MCU's bytes instead of silently dropping them.
    #[test]
    fn cdc_vcom_safe_assert_sets_both_lines_high() {
        let mut port = RecordedPort::default();
        cdc_vcom_safe_assert(&mut port).expect("cdc_vcom_safe_assert should succeed");
        assert_eq!(
            port.events,
            vec![("DTR", true), ("RTS", true)],
            "CDC-ACM bridges require DTR=true and RTS=true for the bridge \
             to forward bytes from the target MCU"
        );
    }

    #[test]
    fn cdc_vcom_safe_assert_dtr_failure_short_circuits_before_rts() {
        let mut port = RecordedPort {
            fail_on: Some("dtr"),
            ..RecordedPort::default()
        };
        assert!(cdc_vcom_safe_assert(&mut port).is_err());
        assert!(
            port.events.is_empty(),
            "an error on the DTR write must NOT proceed to the RTS write",
        );
    }

    #[test]
    fn cdc_vcom_safe_assert_rts_failure_surfaces() {
        let mut port = RecordedPort {
            fail_on: Some("rts"),
            ..RecordedPort::default()
        };
        assert!(cdc_vcom_safe_assert(&mut port).is_err());
    }
}
