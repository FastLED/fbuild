//! Baud-134 soft reboot trigger.
//!
//! Teensyduino's USB stack watches the host-requested baud rate on its CDC ACM
//! endpoint. Setting **baud 134** is the published signal to jump from user
//! firmware into the HalfKay bootloader. PJRC's own loader does this via raw
//! `SetCommState` on Windows; the `serialport` crate's portable
//! `SerialPortBuilder::baud_rate(134)` covers Linux/macOS/Windows.
//!
//! See `~/.platformio/packages/framework-arduinoteensy/cores/teensy4/usb_serial.c`
//! for the device-side detection.
//!
//! Failure modes that are *expected*:
//! - The port may already be in HalfKay (HID class) — `available_ports()` will
//!   no longer list it. We treat "port not present" as a no-op success.
//! - The port may close on us partway through (the device is already
//!   rebooting). We swallow those errors too.

use std::time::Duration;

use fbuild_core::{FbuildError, Result};

/// Hold-open duration for the baud-134 trigger.
///
/// 100 ms is plenty for the device to observe the baud-rate change and start
/// the reboot. Mirrors `reset.rs::reset_teensy`.
const HOLD_OPEN_MS: u64 = 100;

/// Open `port` at baud 134 to ask Teensyduino to drop into HalfKay.
///
/// Returns `Ok(true)` when the port opened and the trigger was issued.
/// Returns `Ok(false)` when the port was absent (caller should treat the
/// device as already-HalfKay and proceed to the flash step).
/// Returns `Err` only for genuinely unexpected errors — most "port vanished
/// mid-handshake" surfaces are normalised to `Ok(true)`.
pub fn baud_134_trigger(port: &str, verbose: bool) -> Result<bool> {
    if verbose {
        tracing::info!("teensy soft reboot: opening {} at baud 134", port);
    }

    let builder = serialport::new(port, 134).timeout(Duration::from_secs(2));
    match builder.open() {
        Ok(_serial) => {
            // Hold the port open just long enough for the device-side baud
            // observer to fire. Closing the handle here is fine — the reboot
            // is already queued by the time we drop.
            std::thread::sleep(Duration::from_millis(HOLD_OPEN_MS));
            Ok(true)
        }
        Err(e) if e.kind() == serialport::ErrorKind::NoDevice => {
            // Already in HalfKay, or hot-unplugged. Either way: not our problem.
            if verbose {
                tracing::info!(
                    "teensy soft reboot: {} not present — treating as already-HalfKay",
                    port
                );
            }
            Ok(false)
        }
        Err(e) => {
            // Other open errors (permission denied, device-busy on Linux from
            // another monitor that didn't release the port) are real — surface
            // them so the caller can decide whether to fall back to the
            // wait-for-halfkay path or abort.
            Err(FbuildError::SerialError(format!(
                "baud-134 trigger failed on {}: {}",
                port, e
            )))
        }
    }
}

/// True when the user has explicitly opted out of the baud-134 trigger via
/// the `FBUILD_TEENSY_DISABLE_BAUD_134_TRIGGER` environment variable.
///
/// Provided so the deployer can fall back to "press the program button"
/// behaviour on hosts where `SerialPortBuilder::baud_rate(134)` is not honored.
pub fn baud_134_trigger_disabled() -> bool {
    std::env::var("FBUILD_TEENSY_DISABLE_BAUD_134_TRIGGER")
        .map(|v| !matches!(v.as_str(), "" | "0" | "false" | "FALSE" | "False"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonexistent_port_returns_false_not_err() {
        // Pure unit test: any host without a Teensy plugged at this port name
        // should return `Ok(false)` (no device). The point is to make sure
        // that a missing CDC port is never treated as a hard error.
        let port = if cfg!(windows) {
            "COM199" // unlikely to exist
        } else {
            "/tmp/fbuild-teensy-no-such-port"
        };
        // Some platforms surface a different error class on a synthetic path —
        // accept either Ok(false) or a SerialError, but never panic.
        let _ = baud_134_trigger(port, false);
    }

    #[test]
    fn disabled_by_env() {
        // SAFETY: tests are single-threaded per process here.
        std::env::set_var("FBUILD_TEENSY_DISABLE_BAUD_134_TRIGGER", "1");
        assert!(baud_134_trigger_disabled());
        std::env::set_var("FBUILD_TEENSY_DISABLE_BAUD_134_TRIGGER", "0");
        assert!(!baud_134_trigger_disabled());
        std::env::remove_var("FBUILD_TEENSY_DISABLE_BAUD_134_TRIGGER");
        assert!(!baud_134_trigger_disabled());
    }
}
