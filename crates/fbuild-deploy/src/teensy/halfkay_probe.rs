//! Wait for HalfKay to be ready.
//!
//! HalfKay is the Teensy bootloader USB HID class device. The `serialport`
//! crate enumerates *CDC ACM serial* ports — it will not list HalfKay. So we
//! cannot directly detect HalfKay through serialport.
//!
//! What we *can* detect is the CDC ACM port **disappearing**. After a
//! successful baud-134 trigger the user firmware re-enumerates as HID (HalfKay)
//! and its old CDC name leaves `available_ports()`. That's the strongest
//! cross-platform signal we have that the reboot took.
//!
//! For the fresh-board case (no pre-flash CDC port at all because the device
//! is already in HalfKay, or because user firmware was never installed), we
//! delegate the wait to `teensy_loader_cli -w` (handled by the `flash.rs`
//! module). This module's `wait_for_disappearance` is purely a confirmation
//! step for the baud-134 path.

use std::time::{Duration, Instant};

use super::port_discovery::list_ports;

/// Outcome of [`wait_for_disappearance`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisappearOutcome {
    /// `port` was not in `available_ports()` at some poll within `timeout`.
    /// Strong signal the device dropped its CDC class and is now in HalfKay
    /// (HID class) or completing re-enumeration.
    Gone,
    /// The port was still enumerated when `timeout` expired. The caller may
    /// still proceed to `teensy_loader_cli -w`, which has its own
    /// HID-level wait, but should log a warning so a user with a stuck device
    /// gets a clue.
    Present,
}

/// Poll `available_ports()` and return as soon as `port` is no longer present,
/// or after `timeout` elapses.
///
/// Poll cadence is 75 ms — slightly tighter than [`super::port_discovery`]
/// because the disappearance window is short (~250 ms on Teensy 4) and
/// missing it would lengthen the perceived latency of every deploy.
pub fn wait_for_disappearance(port: &str, timeout: Duration) -> DisappearOutcome {
    let deadline = Instant::now() + timeout;
    let poll = Duration::from_millis(75);
    loop {
        let present = list_ports().iter().any(|info| info.port_name == port);
        if !present {
            return DisappearOutcome::Gone;
        }
        if Instant::now() >= deadline {
            return DisappearOutcome::Present;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_port_reports_gone_immediately() {
        // A port that never existed is, by construction, "gone".
        let outcome = wait_for_disappearance("/dev/no-such-teensy", Duration::from_millis(50));
        assert_eq!(outcome, DisappearOutcome::Gone);
    }
}
