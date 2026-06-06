//! Advisory probe that asks "did the firmware produce any serial output?"
//!
//! Even after `teensy_loader_cli` reports `Booting` and the new CDC ACM port
//! enumerates, the firmware could be hanging in `setup()` before its first
//! `Serial.print`. The user then sees `fbuild deploy: SUCCESS` but their
//! monitor drains zero bytes forever.
//!
//! This probe is purely advisory. It does NOT gate deploy success. It just
//! gives the user a structured hint when the post-deploy silence is suspicious
//! (most likely culprits: a sketch built with `usb_type=USB_MIDI_SERIAL`, a
//! `Serial.begin(115200)` that never returns, or a crash in `setup()` before
//! any output).

use std::io::Read;
use std::time::{Duration, Instant};

/// Outcome of [`probe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstByteOutcome {
    /// At least one byte arrived within the budget. `elapsed_ms` is the wall
    /// time from probe start to the first byte.
    SawByte { elapsed_ms: u64 },
    /// The full budget elapsed without any byte arriving. `port_opened` is
    /// `true` if we successfully opened the port (a `false` here means the
    /// device disappeared before we could attach).
    Silent { port_opened: bool },
    /// The probe was disabled (timeout 0 ms). The caller should treat this
    /// the same as "no probe ran" — not a success, not a failure.
    Disabled,
}

impl FirstByteOutcome {
    /// `true` if the outcome warrants a louder diagnostic from the deployer.
    pub fn is_suspicious(&self) -> bool {
        matches!(self, FirstByteOutcome::Silent { .. })
    }
}

/// Open `port` at `baud` and read until either a byte arrives or `timeout`
/// elapses.
///
/// The internal per-read timeout is short (100 ms) so the probe surfaces a
/// byte as soon as it lands, without busy-spinning the CPU.
pub fn probe(port: &str, baud: u32, timeout: Duration) -> FirstByteOutcome {
    if timeout.is_zero() {
        return FirstByteOutcome::Disabled;
    }

    let start = Instant::now();
    let builder = serialport::new(port, baud).timeout(Duration::from_millis(100));
    let mut serial = match builder.open() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("first-byte probe: failed to open {}: {}", port, e);
            return FirstByteOutcome::Silent { port_opened: false };
        }
    };

    let mut buf = [0u8; 64];
    while Instant::now().duration_since(start) < timeout {
        match serial.read(&mut buf) {
            Ok(n) if n > 0 => {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                return FirstByteOutcome::SawByte { elapsed_ms };
            }
            // n == 0 is "EOF" from `Read` semantics; for serial this just
            // means "no bytes ready in the last 100 ms" — keep polling.
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => {
                tracing::warn!("first-byte probe: read error on {}: {}", port, e);
                return FirstByteOutcome::Silent { port_opened: true };
            }
        }
    }
    FirstByteOutcome::Silent { port_opened: true }
}

/// `FBUILD_TEENSY_FIRST_BYTE_TIMEOUT_SECS` override, if set.
/// `0` is a valid value and disables the probe.
pub fn env_first_byte_timeout_secs_override() -> Option<u64> {
    std::env::var("FBUILD_TEENSY_FIRST_BYTE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
}

/// Human-readable diagnostic text for a `Silent` outcome.
///
/// Wired through to `DeploymentResult.message` (as a suffix) so the user
/// sees the actionable hint inline with the deploy log instead of having
/// to grep for "Teensy enumerated as".
pub fn silent_diagnostic(port: &str, timeout_secs: u64) -> String {
    format!(
        "Teensy enumerated as {port} but produced zero serial bytes in {timeout_secs}s. \
         The firmware may be hanging in setup(). Try: \
         (a) press the program button to re-enter HalfKay and reflash; \
         (b) check the build's usb_type (USB_SERIAL vs USB_MIDI_SERIAL); \
         (c) verify your sketch's Serial.begin(115200) runs to completion."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_when_timeout_zero() {
        let outcome = probe("/dev/no-such-port", 115_200, Duration::ZERO);
        assert_eq!(outcome, FirstByteOutcome::Disabled);
    }

    #[test]
    fn open_failure_reports_port_not_opened() {
        // A path that can never be opened as a serial port — verifies the
        // open-error branch produces the right outcome variant.
        let outcome = probe(
            "/this/path/does/not/exist/as/serial",
            115_200,
            Duration::from_millis(50),
        );
        assert!(matches!(
            outcome,
            FirstByteOutcome::Silent { port_opened: false }
        ));
        assert!(outcome.is_suspicious());
    }

    #[test]
    fn env_override_parses_zero() {
        let _guard = crate::teensy::soft_reboot::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("FBUILD_TEENSY_FIRST_BYTE_TIMEOUT_SECS", "0");
        assert_eq!(env_first_byte_timeout_secs_override(), Some(0));
        std::env::set_var("FBUILD_TEENSY_FIRST_BYTE_TIMEOUT_SECS", "30");
        assert_eq!(env_first_byte_timeout_secs_override(), Some(30));
        std::env::set_var("FBUILD_TEENSY_FIRST_BYTE_TIMEOUT_SECS", "junk");
        assert_eq!(env_first_byte_timeout_secs_override(), None);
        std::env::remove_var("FBUILD_TEENSY_FIRST_BYTE_TIMEOUT_SECS");
        assert_eq!(env_first_byte_timeout_secs_override(), None);
    }

    #[test]
    fn silent_diagnostic_mentions_port_and_timeout() {
        let msg = silent_diagnostic("COM7", 10);
        assert!(msg.contains("COM7"));
        assert!(msg.contains("10s"));
        assert!(msg.contains("program button"));
    }
}
