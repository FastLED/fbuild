//! Board fingerprint registry — vendored from FastLED/FastLED#3339's
//! `ci/util/serial_probe.py::BOARD_FINGERPRINTS` and
//! `ci/util/port_utils.py::ENVIRONMENT_TO_VCOM_VID_PID`.
//!
//! FastLED/fbuild#686 brings the same data and the matching
//! introspection APIs into fbuild so port-probe tooling on either side
//! of the FastLED ↔ fbuild boundary speaks from one source of truth.
//! See the issue for the cross-repo-sync acceptance criterion.
//!
//! # What lives here
//!
//! - [`BOARD_FINGERPRINTS`] — every `(vid, pid)` we know how to
//!   describe in human-readable form. Looked up by [`board_hint`].
//! - [`ENVIRONMENT_TO_VCOM`] — PlatformIO-style env names ("lpc845brk",
//!   "esp32dev") mapped to the `(vid, pid)` of the USB-VCOM bridge
//!   that's the right port to talk to on that board. Looked up by
//!   [`vcom_for_env`].
//! - [`BoardFamily`] — the DTR/RTS-convention taxonomy hinted at by
//!   FastLED/fbuild#684's closing comment and used by every probe /
//!   monitor open path to pick the right [`idle_dtr_rts`] tuple
//!   instead of falling back to the ESP "DTR=low, RTS=low" default
//!   that silently drops bytes on CDC-ACM bridges. (`BoardFamily`
//!   here is intentionally minimal — the polymorphic `ResetMethod`
//!   dispatch registry it eventually feeds is the scope of
//!   FastLED/fbuild#687.)
//!
//! [`idle_dtr_rts`]: BoardFamily::idle_dtr_rts

/// Human-readable description of every `(vid, pid)` we recognize.
///
/// Order is arbitrary — lookups linear-scan. The table is small
/// enough (~10s of entries) that a hash map or `phf` would be more
/// ceremony than payoff. Add entries here; do NOT add a parallel
/// table elsewhere in the crate.
///
/// Vendored from FastLED/FastLED#3339's `BOARD_FINGERPRINTS`.
pub const BOARD_FINGERPRINTS: &[(u16, u16, &str)] = &[
    // NXP — LPC8xx CMSIS-DAP debug + LPC11U35 VCOM bridge
    (
        0x1FC9,
        0x0132,
        "NXP CMSIS-DAP debug (LPC845-BRK / LPC11U35)",
    ),
    (
        0x16C0,
        0x0483,
        "LPC11U35 VCOM bridge (LPC845-BRK USART0) OR PJRC Teensy USB-Serial",
    ),
    // Espressif — native USB CDC (ESP32-S3/C3/C6/H2/P4)
    (
        0x303A,
        0x1001,
        "Espressif native USB CDC (ESP32-S3/C3/C6/H2/P4)",
    ),
    (0x303A, 0x0002, "Espressif USB JTAG/serial debug unit"),
    // Silicon Labs — CP210x USB-UART (common on ESP32 dev kits)
    (
        0x10C4,
        0xEA60,
        "Silicon Labs CP2102 USB-UART (ESP32 dev kit)",
    ),
    (0x10C4, 0xEA70, "Silicon Labs CP2105 dual USB-UART"),
    // WCH — CH340/CH341 USB-Serial (very common on cheap ESP32 / Arduino clones)
    (0x1A86, 0x7523, "WCH CH340 USB-Serial"),
    (0x1A86, 0x55D4, "WCH CH9102 USB-Serial"),
    // FTDI — FT232R / FT231X
    (0x0403, 0x6001, "FTDI FT232R USB-UART"),
    (0x0403, 0x6015, "FTDI FT231X USB-UART"),
    // Arduino — official boards
    (0x2341, 0x0043, "Arduino Uno R3"),
    (0x2341, 0x0001, "Arduino Uno"),
    (0x2341, 0x0010, "Arduino Mega 2560"),
    (0x2341, 0x804E, "Arduino Zero (Native USB)"),
    // RP2040 / Raspberry Pi Pico
    (0x2E8A, 0x000A, "Raspberry Pi Pico (USB CDC)"),
    (0x2E8A, 0x0003, "Raspberry Pi Pico (BOOTSEL)"),
];

/// PlatformIO-style environment / board names → the `(vid, pid)` of
/// the USB-VCOM bridge that's the right port to talk to on that
/// board.
///
/// Used by `fbuild serial probe find --env <env>` to disambiguate
/// which of several serial enumerations belongs to the target board
/// — the LPC845-BRK enumerates TWO USB devices (CMSIS-DAP debug AND
/// the LPC11U35 VCOM bridge) and only the second is what fbuild
/// monitor / esptool / pyOCD wants. Vendored from
/// FastLED/FastLED#3339's `ENVIRONMENT_TO_VCOM_VID_PID`.
pub const ENVIRONMENT_TO_VCOM: &[(&str, u16, u16)] = &[
    // LPC845-BRK and siblings — VCOM bridge is the LPC11U35
    ("lpc845brk", 0x16C0, 0x0483),
    ("lpc845", 0x16C0, 0x0483),
    ("lpc804", 0x16C0, 0x0483),
    ("lpcxpresso845max", 0x16C0, 0x0483),
    ("lpcxpresso804", 0x16C0, 0x0483),
];

/// Family-of-boards taxonomy used to pick correct DTR/RTS conventions
/// on port open + post-reset paths.
///
/// FastLED/fbuild#684's closing analysis identified this as the right
/// abstraction; this enum is the minimum needed to satisfy #686's
/// fourth acceptance criterion ("the probe API picks the right DTR/RTS
/// for the target board family"). The full polymorphic dispatch
/// registry that this enum eventually backs (`ResetMethod`,
/// `BootModeClassifier`, `HandoffTiming`) is the scope of
/// FastLED/fbuild#687, #688, and #691 respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardFamily {
    /// ESP32 native USB CDC (ESP32-S3/C3/C6/H2/P4). The chip enumerates
    /// directly to the host. Reset via DTR/RTS pulse.
    /// Post-reset idle: DTR=false, RTS=false (BOOT high, EN high → run firmware).
    Esp32NativeUsbCdc,

    /// ESP32 via external USB-UART chip (CP2102 / CH340 / FTDI on a
    /// classic DevKit-V1). Same DTR/RTS convention as
    /// [`Self::Esp32NativeUsbCdc`] — the bridge chip exposes the same
    /// line-control bits to the host even though the path is
    /// host → CP2102 → ESP32 UART instead of host → ESP32 directly.
    Esp32ExternalUart,

    /// Bridge-based USB VCOM (LPC11U35 on LPC845-BRK, RP2040 board
    /// bridges). Reset is via SWD/CMSIS-DAP, NOT DTR/RTS — calling
    /// [`crate::esp_reset::esp_hard_reset_blocking`] on these boards
    /// leaves DTR=low which the bridge treats as "host not ready"
    /// and silently drops every byte the target MCU transmits
    /// (the FastLED/FastLED#3300 failure).
    /// Post-open idle: DTR=true, RTS=true (host-ready for the bridge).
    CdcAcmBridge,

    /// Generic Arduino path — FTDI / CP2102 / CH340 as the primary
    /// USB endpoint on a board with the Arduino auto-reset capacitor.
    /// DTR-pulse-on-open may reset the chip; the idle state is
    /// DTR=true / RTS=true so the bridge passes target bytes through.
    Arduino,
}

impl BoardFamily {
    /// The universal "host attached, data flow OK" port-open / post-
    /// reset idle DTR/RTS state for this board family.
    ///
    /// Return shape is `(dtr, rts)` — both `true` means "drive the line
    /// high", which depending on the bridge chip is either "host
    /// ready" (CDC-ACM bridge) or "BOOT/EN held high → run firmware"
    /// (ESP via DevKit autoreset).
    ///
    /// # Examples
    ///
    /// ```
    /// use fbuild_serial::boards::BoardFamily;
    ///
    /// assert_eq!(BoardFamily::Esp32NativeUsbCdc.idle_dtr_rts(), (false, false));
    /// assert_eq!(BoardFamily::Esp32ExternalUart.idle_dtr_rts(), (false, false));
    /// assert_eq!(BoardFamily::CdcAcmBridge.idle_dtr_rts(),     (true,  true));
    /// assert_eq!(BoardFamily::Arduino.idle_dtr_rts(),          (true,  true));
    /// ```
    #[must_use]
    pub fn idle_dtr_rts(&self) -> (bool, bool) {
        use BoardFamily::*;
        match self {
            Esp32NativeUsbCdc | Esp32ExternalUart => (false, false),
            CdcAcmBridge | Arduino => (true, true),
        }
    }
}

/// Look up the human-readable hint for a `(vid, pid)` pair.
///
/// Returns `None` when the pair is unknown — the probe should fall
/// through to whatever the OS-provided descriptor string says.
///
/// # Examples
///
/// ```
/// use fbuild_serial::boards::board_hint;
///
/// assert!(board_hint(0x303A, 0x1001)
///     .unwrap()
///     .contains("Espressif"));
/// assert!(board_hint(0x16C0, 0x0483)
///     .unwrap()
///     .contains("LPC11U35"));
/// assert_eq!(board_hint(0xDEAD, 0xBEEF), None);
/// ```
#[must_use]
pub fn board_hint(vid: u16, pid: u16) -> Option<&'static str> {
    BOARD_FINGERPRINTS
        .iter()
        .find_map(|(v, p, hint)| (*v == vid && *p == pid).then_some(*hint))
}

/// Look up the `(vid, pid)` of the USB-VCOM bridge for a given
/// PlatformIO env / board name.
///
/// Returns `None` for any env that doesn't have an explicit override
/// — the typical case (a board whose primary USB endpoint IS the
/// chip itself) needs no override.
///
/// # Examples
///
/// ```
/// use fbuild_serial::boards::vcom_for_env;
///
/// assert_eq!(vcom_for_env("lpc845brk"), Some((0x16C0, 0x0483)));
/// assert_eq!(vcom_for_env("lpc804"),    Some((0x16C0, 0x0483)));
/// assert_eq!(vcom_for_env("esp32dev"),  None);
/// ```
#[must_use]
pub fn vcom_for_env(env: &str) -> Option<(u16, u16)> {
    ENVIRONMENT_TO_VCOM
        .iter()
        .find_map(|(name, v, p)| (*name == env).then_some((*v, *p)))
}

/// Best-effort classification of a board family from a `(vid, pid)`
/// pair.
///
/// Used by probe / monitor open paths that don't know the
/// PlatformIO env but DO know the VID/PID from `serialport`
/// enumeration. Falls through to `None` for unknown pairs; the
/// caller should treat that as "unknown — apply the safe-default
/// CDC-ACM convention (DTR=true, RTS=true)" rather than the
/// historical ESP-default (DTR=false, RTS=false) which is the
/// FastLED/FastLED#3300 silent-byte-drop trap.
///
/// # Examples
///
/// ```
/// use fbuild_serial::boards::{family_for_vid_pid, BoardFamily};
///
/// assert_eq!(family_for_vid_pid(0x303A, 0x1001), Some(BoardFamily::Esp32NativeUsbCdc));
/// assert_eq!(family_for_vid_pid(0x16C0, 0x0483), Some(BoardFamily::CdcAcmBridge));
/// assert_eq!(family_for_vid_pid(0x10C4, 0xEA60), Some(BoardFamily::Esp32ExternalUart));
/// assert_eq!(family_for_vid_pid(0x2341, 0x0043), Some(BoardFamily::Arduino));
/// assert_eq!(family_for_vid_pid(0xDEAD, 0xBEEF), None);
/// ```
#[must_use]
pub fn family_for_vid_pid(vid: u16, pid: u16) -> Option<BoardFamily> {
    use BoardFamily::*;
    match (vid, pid) {
        // Espressif native USB
        (0x303A, _) => Some(Esp32NativeUsbCdc),
        // LPC11U35 VCOM bridge (also matches Teensy USB-Serial; safer to
        // treat as a CDC-ACM bridge in either case — assert DTR=true,
        // RTS=true)
        (0x16C0, 0x0483) => Some(CdcAcmBridge),
        // NXP CMSIS-DAP debug probes — not a data port but if a caller
        // hits this we don't want them assuming ESP defaults
        (0x1FC9, _) => Some(CdcAcmBridge),
        // CP2102 / CH340 / FTDI — almost always paired with an ESP32
        // classic DevKit in the FastLED ecosystem; the chip's RS-232
        // line-control bits drive ESP32 BOOT/EN through the autoreset
        // transistor pair. Classify as Esp32ExternalUart.
        (0x10C4, _) | (0x1A86, _) | (0x0403, _) => Some(Esp32ExternalUart),
        // Arduino official
        (0x2341, _) => Some(Arduino),
        // RP2040 — treat as CDC bridge for the post-open DTR/RTS=true
        // safety default; reset is a 1200-baud touch, not DTR/RTS, so
        // the choice here only matters for "monitor / probe doesn't
        // drop bytes."
        (0x2E8A, _) => Some(CdcAcmBridge),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_hint_known_pairs() {
        assert_eq!(
            board_hint(0x303A, 0x1001),
            Some("Espressif native USB CDC (ESP32-S3/C3/C6/H2/P4)")
        );
        assert_eq!(
            board_hint(0x16C0, 0x0483),
            Some("LPC11U35 VCOM bridge (LPC845-BRK USART0) OR PJRC Teensy USB-Serial")
        );
        assert_eq!(
            board_hint(0x1FC9, 0x0132),
            Some("NXP CMSIS-DAP debug (LPC845-BRK / LPC11U35)")
        );
        assert_eq!(board_hint(0x1A86, 0x7523), Some("WCH CH340 USB-Serial"));
        assert_eq!(board_hint(0x0403, 0x6001), Some("FTDI FT232R USB-UART"));
    }

    #[test]
    fn board_hint_unknown_returns_none() {
        assert_eq!(board_hint(0xDEAD, 0xBEEF), None);
        assert_eq!(board_hint(0, 0), None);
        // Vendor known, product unknown — exact-match policy
        assert_eq!(board_hint(0x303A, 0xFFFF), None);
    }

    #[test]
    fn vcom_for_env_known_lpc_boards() {
        assert_eq!(vcom_for_env("lpc845brk"), Some((0x16C0, 0x0483)));
        assert_eq!(vcom_for_env("lpc845"), Some((0x16C0, 0x0483)));
        assert_eq!(vcom_for_env("lpc804"), Some((0x16C0, 0x0483)));
        assert_eq!(vcom_for_env("lpcxpresso845max"), Some((0x16C0, 0x0483)));
        assert_eq!(vcom_for_env("lpcxpresso804"), Some((0x16C0, 0x0483)));
    }

    #[test]
    fn vcom_for_env_returns_none_for_envs_without_override() {
        // Most envs don't need a VCOM override — the primary USB
        // endpoint IS the chip.
        assert_eq!(vcom_for_env("esp32dev"), None);
        assert_eq!(vcom_for_env("esp32s3"), None);
        assert_eq!(vcom_for_env("uno"), None);
        assert_eq!(vcom_for_env(""), None);
        assert_eq!(vcom_for_env("some-random-string"), None);
    }

    #[test]
    fn idle_dtr_rts_esp_families_are_false_false() {
        assert_eq!(
            BoardFamily::Esp32NativeUsbCdc.idle_dtr_rts(),
            (false, false)
        );
        assert_eq!(
            BoardFamily::Esp32ExternalUart.idle_dtr_rts(),
            (false, false)
        );
    }

    #[test]
    fn idle_dtr_rts_cdc_and_arduino_are_true_true() {
        // FastLED/FastLED#3300 mirror: CDC-ACM bridges (LPC11U35, FTDI
        // CDC) MUST see DTR=true and RTS=true to forward target-MCU
        // bytes. Pin this invariant.
        assert_eq!(BoardFamily::CdcAcmBridge.idle_dtr_rts(), (true, true));
        assert_eq!(BoardFamily::Arduino.idle_dtr_rts(), (true, true));
    }

    #[test]
    fn family_for_vid_pid_classifies_known_devices() {
        // Espressif native USB
        assert_eq!(
            family_for_vid_pid(0x303A, 0x1001),
            Some(BoardFamily::Esp32NativeUsbCdc)
        );
        assert_eq!(
            family_for_vid_pid(0x303A, 0x0002),
            Some(BoardFamily::Esp32NativeUsbCdc)
        );

        // LPC11U35 VCOM bridge → CDC-ACM
        assert_eq!(
            family_for_vid_pid(0x16C0, 0x0483),
            Some(BoardFamily::CdcAcmBridge)
        );

        // NXP CMSIS-DAP → also CDC-ACM (safe default)
        assert_eq!(
            family_for_vid_pid(0x1FC9, 0x0132),
            Some(BoardFamily::CdcAcmBridge)
        );

        // CP2102 / CH340 / FTDI on classic ESP DevKit
        assert_eq!(
            family_for_vid_pid(0x10C4, 0xEA60),
            Some(BoardFamily::Esp32ExternalUart)
        );
        assert_eq!(
            family_for_vid_pid(0x1A86, 0x7523),
            Some(BoardFamily::Esp32ExternalUart)
        );
        assert_eq!(
            family_for_vid_pid(0x0403, 0x6001),
            Some(BoardFamily::Esp32ExternalUart)
        );

        // Arduino official
        assert_eq!(
            family_for_vid_pid(0x2341, 0x0043),
            Some(BoardFamily::Arduino)
        );

        // RP2040 / Pico → CDC bridge
        assert_eq!(
            family_for_vid_pid(0x2E8A, 0x000A),
            Some(BoardFamily::CdcAcmBridge)
        );
    }

    #[test]
    fn family_for_vid_pid_returns_none_for_unknown() {
        assert_eq!(family_for_vid_pid(0xDEAD, 0xBEEF), None);
        assert_eq!(family_for_vid_pid(0, 0), None);
    }

    /// The whole point of #684 + #686: any path that ends in
    /// `idle_dtr_rts()` getting `(false, false)` on a CDC-ACM bridge
    /// reintroduces the FastLED/FastLED#3300 silent-byte-drop bug.
    /// Pin the mapping CDC bridge VID/PIDs end at the host-ready idle.
    #[test]
    fn cdc_bridge_vid_pids_resolve_to_host_ready_idle() {
        // LPC11U35 VCOM bridge (LPC845-BRK)
        let lpc = family_for_vid_pid(0x16C0, 0x0483).unwrap();
        assert_eq!(lpc.idle_dtr_rts(), (true, true));

        // RP2040 / Pico
        let pico = family_for_vid_pid(0x2E8A, 0x000A).unwrap();
        assert_eq!(pico.idle_dtr_rts(), (true, true));
    }
}
