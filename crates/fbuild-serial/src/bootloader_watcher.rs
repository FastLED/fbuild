//! USB-level bootloader re-enumeration detection.
//!
//! FastLED/fbuild#693 — complement to FastLED/fbuild#688's
//! `BootModeClassifier`. The classifier covers wedges visible on the
//! serial line; this module covers transitions where the board
//! disappears from the host's USB tree as one device and reappears
//! as a different one (different VID/PID, different interface
//! class).
//!
//! ## Concretely
//!
//! - **RP2040 BOOTSEL** — board disappears as a CDC serial device
//!   and reappears as USB MSC (`2E8A:0003`). Without USB-level
//!   awareness, fbuild sees "monitor died," retries, and never
//!   notices the bootloader is open.
//! - **SAMD21/SAMD51 DFU** — native USB CDC drops; USB DFU
//!   interface appears (`03EB:6124`).
//! - **Teensy HID bootloader** — serial drops, HID interface
//!   appears (`16C0:0478`).
//!
//! ## What this module provides
//!
//! - [`BootloaderSignature`] — the typed thing the caller is
//!   looking for after triggering a reset.
//! - [`PortSource`] trait — abstracts `serialport::available_ports`
//!   so the watcher's tests can drive the port list deterministically.
//! - [`watch_for_bootloader`] — poll-based, returns the matching
//!   port info as soon as the signature appears, errors after
//!   `timeout` elapses.
//!
//! ## Acceptance criterion 3 — caller integration
//!
//! `ResetMethod::TouchBaud1200` consumers (Teensy / SAMD / RP2040
//! `Deployer` impls) call this immediately after the 1200-baud
//! touch closes the port. Failure to observe the bootloader within
//! `timeout` is a typed error the deployer surfaces instead of
//! silently retrying.

use std::time::{Duration, Instant};

use crate::boards::BoardFamily;

/// USB bootloader interface the watcher knows how to recognize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootloaderSignature {
    /// RP2040 BOOTSEL — USB MSC interface at `2E8A:0003`.
    Rp2040BootSel,
    /// SAMD21/SAMD51 DFU — Atmel USB DFU at `03EB:6124`.
    SamdDfu,
    /// Adafruit / arduino UF2 bootloader — VID 0x239A. PID varies
    /// per board family; classify by VID for a permissive match.
    SamdUf2,
    /// Teensy HID bootloader (HalfKay) — `16C0:0478`.
    TeensyHidBootloader,
}

impl BootloaderSignature {
    /// Return `true` if the given `(vid, pid)` matches this signature.
    /// `SamdUf2` is VID-only (PID varies); the others are exact-pair.
    #[must_use]
    pub fn matches(&self, vid: u16, pid: u16) -> bool {
        match self {
            BootloaderSignature::Rp2040BootSel => (vid, pid) == (0x2E8A, 0x0003),
            BootloaderSignature::SamdDfu => (vid, pid) == (0x03EB, 0x6124),
            BootloaderSignature::SamdUf2 => vid == 0x239A,
            BootloaderSignature::TeensyHidBootloader => (vid, pid) == (0x16C0, 0x0478),
        }
    }

    /// The signature corresponding to this family's reset, if any.
    ///
    /// `BoardFamily::NativeUsbCdcReset1200Bps` is ambiguous — could
    /// be RP2040 or SAMD UF2. Return `Rp2040BootSel` as the most-
    /// common case; the caller has the VID-PID from the prior CDC
    /// enumeration if they need the exact pick.
    #[must_use]
    pub fn for_family(family: BoardFamily) -> Option<BootloaderSignature> {
        use BoardFamily::*;
        match family {
            Teensy => Some(Self::TeensyHidBootloader),
            NativeUsbCdcReset1200Bps => Some(Self::Rp2040BootSel),
            _ => None,
        }
    }
}

/// A lightweight `(vid, pid, name)` snapshot of one USB serial /
/// MSC / HID port. Decoupled from `serialport::SerialPortInfo` so
/// tests can drive the source without standing up a real port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortFingerprint {
    pub vid: u16,
    pub pid: u16,
    pub name: String,
}

/// Source of USB-port snapshots. Implementations:
///
/// - [`SerialPortSource`] in production — calls
///   `serialport::available_ports`.
/// - Test impls in `#[cfg(test)]` that drive a scripted sequence.
pub trait PortSource {
    fn snapshot(&self) -> Vec<PortFingerprint>;
}

/// Production `PortSource` backed by `serialport::available_ports`.
pub struct SerialPortSource;

impl PortSource for SerialPortSource {
    fn snapshot(&self) -> Vec<PortFingerprint> {
        match serialport::available_ports() {
            Ok(ports) => ports
                .into_iter()
                .filter_map(|port| {
                    if let serialport::SerialPortType::UsbPort(info) = port.port_type {
                        Some(PortFingerprint {
                            vid: info.vid,
                            pid: info.pid,
                            name: port.port_name,
                        })
                    } else {
                        None
                    }
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Outcome of the watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchOutcome {
    /// Bootloader observed. Carries the matching port fingerprint so
    /// the caller can route the next step (`picotool load` against
    /// `port.name` / etc.).
    BootloaderEntered {
        signature: BootloaderSignature,
        port: PortFingerprint,
    },
    /// `timeout` elapsed without the signature appearing. The caller
    /// should surface this as a typed deploy error rather than
    /// silently retrying — the bootloader trigger probably didn't
    /// fire.
    Timeout,
}

/// Configurable knobs for the watcher's poll loop. Defaults match
/// what the production path uses; tests inject smaller values.
#[derive(Debug, Clone, Copy)]
pub struct WatchConfig {
    /// How often to re-snapshot the port list.
    pub poll_interval: Duration,
    /// Total wall-clock budget before reporting `Timeout`.
    pub timeout: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        // 100 ms poll covers the 1200-bps-touch + re-enumeration
        // window well; 5 s timeout matches `HandoffTiming` for the
        // 1200-bps-reset families (FastLED/fbuild#691).
        Self {
            poll_interval: Duration::from_millis(100),
            timeout: Duration::from_millis(5000),
        }
    }
}

/// Poll `source` until a port matching `signature` appears, or
/// `config.timeout` elapses.
///
/// FastLED/fbuild#693. Designed to be called from `spawn_blocking`
/// after the 1200-bps-touch reset closes the CDC port — the watcher
/// itself is purely synchronous (no tokio dependency).
///
/// The poll loop is **edge-detecting on signature match, not on
/// port-set change**: a port that was already present at the first
/// snapshot but matches the signature still wins. That's correct
/// for the typical case (CDC drops between snapshots, MSC appears
/// on the next snapshot), and it also covers the edge case where
/// the bootloader was already present when the caller started
/// watching.
pub fn watch_for_bootloader<S: PortSource>(
    source: &S,
    signature: BootloaderSignature,
    config: WatchConfig,
) -> WatchOutcome {
    let deadline = Instant::now() + config.timeout;
    loop {
        for port in source.snapshot() {
            if signature.matches(port.vid, port.pid) {
                return WatchOutcome::BootloaderEntered { signature, port };
            }
        }
        if Instant::now() >= deadline {
            return WatchOutcome::Timeout;
        }
        std::thread::sleep(config.poll_interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Scripted source — returns the snapshot at index `i` on the
    /// i'th call. Tests use this to model "port appears at T+1
    /// poll" / "never appears" / "already present at T=0."
    struct ScriptedSource {
        snapshots: Vec<Vec<PortFingerprint>>,
        index: Cell<usize>,
    }

    impl ScriptedSource {
        fn new(snapshots: Vec<Vec<PortFingerprint>>) -> Self {
            Self {
                snapshots,
                index: Cell::new(0),
            }
        }
    }

    impl PortSource for ScriptedSource {
        fn snapshot(&self) -> Vec<PortFingerprint> {
            let i = self.index.get();
            let snap = self.snapshots.get(i).cloned().unwrap_or_default();
            self.index.set(i + 1);
            snap
        }
    }

    fn pico_bootloader() -> PortFingerprint {
        PortFingerprint {
            vid: 0x2E8A,
            pid: 0x0003,
            name: "USB MSC".to_string(),
        }
    }

    fn pico_app() -> PortFingerprint {
        PortFingerprint {
            vid: 0x2E8A,
            pid: 0x000A,
            name: "COM20".to_string(),
        }
    }

    // ─── BootloaderSignature::matches ──────────────────────────────

    #[test]
    fn rp2040_bootsel_matches_exact_vidpid() {
        assert!(BootloaderSignature::Rp2040BootSel.matches(0x2E8A, 0x0003));
        // Pico app VID/PID does NOT match bootloader.
        assert!(!BootloaderSignature::Rp2040BootSel.matches(0x2E8A, 0x000A));
    }

    #[test]
    fn samd_dfu_matches_atmel_pair() {
        assert!(BootloaderSignature::SamdDfu.matches(0x03EB, 0x6124));
        assert!(!BootloaderSignature::SamdDfu.matches(0x03EB, 0x6125));
    }

    #[test]
    fn samd_uf2_matches_any_adafruit_pid() {
        assert!(BootloaderSignature::SamdUf2.matches(0x239A, 0x0001));
        assert!(BootloaderSignature::SamdUf2.matches(0x239A, 0x002B));
        assert!(!BootloaderSignature::SamdUf2.matches(0x2341, 0x0001));
    }

    #[test]
    fn teensy_hid_bootloader_matches_pjrc_pair() {
        assert!(BootloaderSignature::TeensyHidBootloader.matches(0x16C0, 0x0478));
        // Teensy USB-Serial VID:PID is 0x16C0:0483 — must NOT match
        // the bootloader signature.
        assert!(!BootloaderSignature::TeensyHidBootloader.matches(0x16C0, 0x0483));
    }

    #[test]
    fn signature_for_family_picks_expected_default() {
        assert_eq!(
            BootloaderSignature::for_family(BoardFamily::Teensy),
            Some(BootloaderSignature::TeensyHidBootloader),
        );
        assert_eq!(
            BootloaderSignature::for_family(BoardFamily::NativeUsbCdcReset1200Bps),
            Some(BootloaderSignature::Rp2040BootSel),
        );
        assert_eq!(
            BootloaderSignature::for_family(BoardFamily::Esp32NativeUsbCdc),
            None,
        );
    }

    // ─── watch_for_bootloader poll loop ────────────────────────────

    /// Bootloader already present in the first snapshot → instant
    /// win. Edge case for "watcher started after the 1200-bps touch
    /// already fired and the host re-enumerated."
    #[test]
    fn watcher_returns_immediately_when_bootloader_already_present() {
        let source = ScriptedSource::new(vec![vec![pico_bootloader()]]);
        let outcome = watch_for_bootloader(
            &source,
            BootloaderSignature::Rp2040BootSel,
            WatchConfig {
                poll_interval: Duration::from_millis(1),
                timeout: Duration::from_millis(100),
            },
        );
        match outcome {
            WatchOutcome::BootloaderEntered { port, signature } => {
                assert_eq!(signature, BootloaderSignature::Rp2040BootSel);
                assert_eq!(port.vid, 0x2E8A);
                assert_eq!(port.pid, 0x0003);
            }
            WatchOutcome::Timeout => panic!("expected BootloaderEntered, got Timeout"),
        }
    }

    /// Bootloader appears on the second snapshot — covers
    /// "CDC drops between snapshots, MSC appears on the next." The
    /// canonical 1200-bps-touch flow.
    #[test]
    fn watcher_returns_when_bootloader_appears_on_later_poll() {
        let source = ScriptedSource::new(vec![
            vec![pico_app()],        // T=0: app still enumerated
            vec![],                  // T=1: USB drops (port set is empty)
            vec![pico_bootloader()], // T=2: bootloader appears
        ]);
        let outcome = watch_for_bootloader(
            &source,
            BootloaderSignature::Rp2040BootSel,
            WatchConfig {
                poll_interval: Duration::from_millis(1),
                timeout: Duration::from_millis(100),
            },
        );
        match outcome {
            WatchOutcome::BootloaderEntered { signature, port } => {
                assert_eq!(signature, BootloaderSignature::Rp2040BootSel);
                assert_eq!(port.name, "USB MSC");
            }
            WatchOutcome::Timeout => panic!("expected BootloaderEntered, got Timeout"),
        }
    }

    /// Bootloader never appears → Timeout. The deploy layer surfaces
    /// this as a typed error instead of silently retrying.
    #[test]
    fn watcher_returns_timeout_when_bootloader_never_appears() {
        let source = ScriptedSource::new(vec![
            vec![pico_app()], // T=0
            vec![pico_app()], // T=1 — still the app
            vec![pico_app()], // T=2
        ]);
        let outcome = watch_for_bootloader(
            &source,
            BootloaderSignature::Rp2040BootSel,
            WatchConfig {
                poll_interval: Duration::from_millis(1),
                // Tight timeout so the test finishes fast.
                timeout: Duration::from_millis(10),
            },
        );
        assert!(matches!(outcome, WatchOutcome::Timeout));
    }

    /// The watcher must not match an unrelated USB device that
    /// happens to be present (a different connected board, an
    /// unrelated USB stick, etc.).
    #[test]
    fn watcher_ignores_unrelated_ports() {
        let source = ScriptedSource::new(vec![
            vec![
                pico_app(),
                PortFingerprint {
                    vid: 0x303A,
                    pid: 0x1001,
                    name: "COM25".to_string(),
                },
            ],
            vec![pico_bootloader()],
        ]);
        let outcome = watch_for_bootloader(
            &source,
            BootloaderSignature::Rp2040BootSel,
            WatchConfig {
                poll_interval: Duration::from_millis(1),
                timeout: Duration::from_millis(100),
            },
        );
        assert!(matches!(outcome, WatchOutcome::BootloaderEntered { .. }));
    }

    /// Default config values are sane (5 s timeout matches
    /// HandoffTiming for 1200-bps families per #691).
    #[test]
    fn default_config_uses_5s_timeout_and_100ms_poll() {
        let cfg = WatchConfig::default();
        assert_eq!(cfg.timeout, Duration::from_millis(5000));
        assert_eq!(cfg.poll_interval, Duration::from_millis(100));
    }
}
