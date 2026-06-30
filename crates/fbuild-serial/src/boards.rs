//! Board fingerprint registry — vendored from FastLED/FastLED#3339's
//! `ci/util/serial_probe.py::BOARD_FINGERPRINTS` and
//! `ci/util/port_utils.py::ENVIRONMENT_TO_VCOM_VID_PID`.
//!
//! FastLED/fbuild#686 brings the same data and the matching
//! introspection APIs into fbuild so port-probe tooling on either side
//! of the FastLED ↔ fbuild boundary speaks from one source of truth.
//! See the issue for the cross-repo-sync acceptance criterion.
//!
//! **For the per-chip DTR/RTS semantics matrix that backs every row in
//! this table — i.e., the *why* behind which family ends up at
//! `(DTR=true, RTS=true)` vs `(false, false)` for its
//! `idle_dtr_rts()` — see `docs/usb-cdc-control-line-matrix.md`
//! (FastLED/fbuild#689).** Every time a new entry is added here, the
//! matrix doc should gain (or already cover) the corresponding chip
//! row.
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
/// abstraction. The enum + [`Self::idle_dtr_rts`] satisfies #686's
/// fourth acceptance criterion ("the probe API picks the right DTR/RTS
/// for the target board family"); the [`Self::reset_method`] +
/// [`crate::esp_reset::dispatch_reset`] pair satisfies #687's
/// polymorphic-dispatch criterion.
///
/// Co-evolving with FastLED/FastLED#3300 / #3325 / #3339 (the LPC845-BRK
/// bring-up incident that cost two debugging sessions because the
/// "is this an ESP or a CDC bridge?" decision had no single point of
/// consultation). See `docs/usb-cdc-control-line-matrix.md` (#689) for
/// the per-chip DTR/RTS semantics this enum encodes.
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

    /// Bridge-based USB VCOM (LPC11U35 on LPC845-BRK, mbed DAPLink
    /// boards). Reset is via SWD/CMSIS-DAP, NOT DTR/RTS — calling
    /// [`crate::esp_reset::esp_hard_reset_blocking`] on these boards
    /// leaves DTR=low which the bridge treats as "host not ready"
    /// and silently drops every byte the target MCU transmits
    /// (the FastLED/FastLED#3300 failure).
    /// Post-open idle: DTR=true, RTS=true (host-ready for the bridge).
    ///
    /// **Future enhancement** (FastLED/fbuild#687 follow-up): carry the
    /// CMSIS-DAP probe's `(vid, pid)` as a payload so the dispatcher
    /// can route SWD reset to the right probe endpoint without an
    /// external lookup. Not in scope for the first cut — adding it
    /// later is a non-breaking enum-variant evolution under the
    /// `#[non_exhaustive]` annotation.
    CdcAcmBridge,

    /// PJRC Teensy 3.x / 4.x. Reset trigger is the "1200-bps touch"
    /// idiom: open port at 1200 baud, close → HalfKay bootloader
    /// engages on disconnect. Post-open idle: DTR=true, RTS=true.
    ///
    /// **Naming-collision caveat:** Teensy enumerates as VID 0x16C0,
    /// PID 0x0483 — *the same pair* as the LPC11U35 VCOM bridge. The
    /// VID/PID lookup in [`family_for_vid_pid`] returns
    /// [`Self::CdcAcmBridge`] for that pair (LPC is the more common
    /// case in this codebase's deployment ecosystem); callers who
    /// know they're on a Teensy must construct [`Self::Teensy`]
    /// explicitly.
    Teensy,

    /// Native USB CDC with a 1200-bps touch reset (SAMD21/SAMD51,
    /// RP2040, Adafruit UF2 boards). The host opens the port at
    /// 1200 baud and closes; the device's TinyUSB stack watches for
    /// the disconnect and reboots into the UF2/BOOTSEL bootloader.
    /// Post-open idle: DTR=true, RTS=true.
    NativeUsbCdcReset1200Bps,

    /// Classic Arduino with capacitor-coupled DTR auto-reset
    /// (UNO / Mega / Nano). Reset trigger is a single
    /// DTR=true→false transition through the 100nF cap on the
    /// ATmega's RESET pin. Post-open idle: DTR=true, RTS=true.
    /// **Be aware of capacitor-charge timing** — opening the port
    /// resets the target by side-effect; the first ~2 s of output
    /// is the bootloader's "wait for upload" window.
    ArduinoAutoReset,
}

impl BoardFamily {
    /// Universal post-attach / post-reset idle state for this family
    /// — what `manager::open_port` sets after acquiring the port
    /// handle.
    ///
    /// Return shape is `(dtr, rts)` — both `true` means "drive the
    /// line high", which depending on the bridge chip is either
    /// "host ready" (CDC-ACM bridge, Teensy, native CDC, Arduino) or
    /// "BOOT/EN held high → run firmware" (ESP via DevKit autoreset).
    ///
    /// # Examples
    ///
    /// ```
    /// use fbuild_serial::boards::BoardFamily;
    ///
    /// assert_eq!(BoardFamily::Esp32NativeUsbCdc.idle_dtr_rts(),         (false, false));
    /// assert_eq!(BoardFamily::Esp32ExternalUart.idle_dtr_rts(),         (false, false));
    /// assert_eq!(BoardFamily::CdcAcmBridge.idle_dtr_rts(),              (true,  true));
    /// assert_eq!(BoardFamily::Teensy.idle_dtr_rts(),                    (true,  true));
    /// assert_eq!(BoardFamily::NativeUsbCdcReset1200Bps.idle_dtr_rts(),  (true,  true));
    /// assert_eq!(BoardFamily::ArduinoAutoReset.idle_dtr_rts(),          (true,  true));
    /// ```
    #[must_use]
    pub fn idle_dtr_rts(&self) -> (bool, bool) {
        use BoardFamily::*;
        match self {
            Esp32NativeUsbCdc | Esp32ExternalUart => (false, false),
            CdcAcmBridge | Teensy | NativeUsbCdcReset1200Bps | ArduinoAutoReset => (true, true),
        }
    }

    /// Reset primitive this family responds to. The companion to
    /// [`Self::idle_dtr_rts`] — `idle_dtr_rts` is the state AFTER a
    /// reset / port-open settles; `reset_method` is HOW the reset
    /// gets triggered.
    ///
    /// FastLED/fbuild#687 — polymorphic dispatch lives in
    /// [`crate::esp_reset::dispatch_reset`] which calls this to pick
    /// the implementation primitive.
    ///
    /// # Examples
    ///
    /// ```
    /// use fbuild_serial::boards::{BoardFamily, ResetMethod};
    ///
    /// assert_eq!(BoardFamily::Esp32NativeUsbCdc.reset_method(),        ResetMethod::DtrRtsPulse);
    /// assert_eq!(BoardFamily::Esp32ExternalUart.reset_method(),        ResetMethod::DtrRtsPulse);
    /// assert_eq!(BoardFamily::CdcAcmBridge.reset_method(),             ResetMethod::SwdViaCmsisDap);
    /// assert_eq!(BoardFamily::Teensy.reset_method(),                   ResetMethod::TouchBaud1200);
    /// assert_eq!(BoardFamily::NativeUsbCdcReset1200Bps.reset_method(), ResetMethod::TouchBaud1200);
    /// assert_eq!(BoardFamily::ArduinoAutoReset.reset_method(),         ResetMethod::DtrPulse);
    /// ```
    #[must_use]
    pub fn reset_method(&self) -> ResetMethod {
        use BoardFamily::*;
        use ResetMethod::*;
        match self {
            Esp32NativeUsbCdc | Esp32ExternalUart => DtrRtsPulse,
            CdcAcmBridge => SwdViaCmsisDap,
            Teensy | NativeUsbCdcReset1200Bps => TouchBaud1200,
            ArduinoAutoReset => DtrPulse,
        }
    }

    /// `true` if this family's reset is driven by serial-port DTR/RTS
    /// (i.e. [`crate::esp_reset::esp_hard_reset_blocking`] /
    /// [`crate::esp_reset::dispatch_reset`] can handle it internally).
    /// Used by `dispatch_reset` to decide between "do the pulse here"
    /// and "return DelegateToCaller so the caller can dispatch SWD /
    /// 1200-bps touch elsewhere."
    #[must_use]
    pub fn reset_is_serial_native(&self) -> bool {
        matches!(self.reset_method(), ResetMethod::DtrRtsPulse)
    }

    /// Per-family flash → monitor handoff timing.
    ///
    /// FastLED/fbuild#691. Numbers track FastLED/FastLED#3339's LPC
    /// bring-up plus observed values from earlier ESP + RP2040 +
    /// Teensy + Arduino sessions. See
    /// `docs/usb-cdc-control-line-matrix.md` (#689) for the per-row
    /// table and citations.
    ///
    /// Consumed by `Deployer::post_deploy_recovery` (#605) instead of
    /// per-deployer inline magic numbers.
    ///
    /// # Examples
    ///
    /// ```
    /// use fbuild_serial::boards::BoardFamily;
    ///
    /// let esp = BoardFamily::Esp32NativeUsbCdc.handoff_timing();
    /// assert_eq!(esp.post_reset_settle_ms, 200);
    /// assert_eq!(esp.boot_drain_ms, 0);
    ///
    /// let lpc = BoardFamily::CdcAcmBridge.handoff_timing();
    /// assert_eq!(lpc.post_reset_settle_ms, 500);
    /// assert_eq!(lpc.boot_drain_ms, 2000);  // LPC11U35 bridge needs the drain
    /// ```
    #[must_use]
    pub fn handoff_timing(&self) -> HandoffTiming {
        use BoardFamily::*;
        match self {
            // ESP32 native + external UART: short settle, no drain
            // (peripheral elides pre-app garbage), 5 retries through
            // CDC re-enum.
            Esp32NativeUsbCdc | Esp32ExternalUart => HandoffTiming {
                post_reset_settle_ms: 200,
                boot_drain_ms: 0,
                port_reappear_timeout_ms: 3000,
                open_retry_count: 5,
            },
            // CDC-ACM bridge (LPC11U35): pyOCD reset settles in ~500
            // ms but the bridge re-emits ~2 s of boot-banner garbage
            // that must be drained before the bring-up RPC. From
            // FastLED/FastLED#3339.
            CdcAcmBridge => HandoffTiming {
                post_reset_settle_ms: 500,
                boot_drain_ms: 2000,
                port_reappear_timeout_ms: 3000,
                open_retry_count: 3,
            },
            // 1200-bps-touch bootloaders (Teensy HalfKay, SAMD UF2,
            // RP2040 BOOTSEL): the port DROPS for ~1-2 s then
            // reappears at the *bootloader* VID/PID, then drops
            // again and reappears at the app VID/PID after flash.
            // Tolerate up to 5 s reappear + 10 open retries to ride
            // out the double-enumeration window.
            Teensy | NativeUsbCdcReset1200Bps => HandoffTiming {
                post_reset_settle_ms: 100,
                boot_drain_ms: 500,
                port_reappear_timeout_ms: 5000,
                open_retry_count: 10,
            },
            // Arduino auto-reset: the bootloader's "wait for upload"
            // window is ~1.5 s — must sleep through it before reading
            // app output. Port doesn't drop (USB endpoint stays on
            // the bridge chip, not the AVR), so reappear timeout is
            // 0 and only 1 open is needed.
            ArduinoAutoReset => HandoffTiming {
                post_reset_settle_ms: 1500,
                boot_drain_ms: 0,
                port_reappear_timeout_ms: 0,
                open_retry_count: 1,
            },
        }
    }
}

/// Flash → monitor handoff timing for a board family.
///
/// FastLED/fbuild#691 — concrete numbers from the FastLED/FastLED#3339
/// LPC845-BRK bring-up incident + observed values across the ESP /
/// Teensy / RP2040 / Arduino ecosystems. Used by
/// `Deployer::post_deploy_recovery` (FastLED/fbuild#605) instead of
/// per-deployer inline magic numbers.
///
/// All fields are `u32` milliseconds; consume via
/// `Duration::from_millis(timing.field)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffTiming {
    /// How long to sleep after `reset` returns before the first byte
    /// is expected on the serial port. Covers the device's boot-ROM
    /// → app-firmware-entry latency.
    pub post_reset_settle_ms: u32,
    /// How long to drain residual boot-banner / boot-up junk before
    /// the bring-up RPC sends its first request. Bridges (LPC11U35)
    /// and stretched-boot devices benefit; ESP native USB is
    /// zero-drain because the CDC peripheral itself elides the
    /// pre-app garbage.
    pub boot_drain_ms: u32,
    /// How long to wait for the port to reappear after the USB endpoint
    /// drops (CDC re-enum, 1200-bps-touch bootloader, BOOTSEL). Set
    /// to `0` for boards that don't drop the port at all (Arduino
    /// classic auto-reset).
    pub port_reappear_timeout_ms: u32,
    /// Max retries on transient open failures during the reappear
    /// window. Higher for boards with longer re-enumeration windows
    /// (Teensy / RP2040 — HalfKay / BOOTSEL).
    pub open_retry_count: u8,
}

/// The hardware primitive that resets a board.
///
/// FastLED/fbuild#687 — the enum that backs polymorphic reset
/// dispatch. Not every variant has an implementation in
/// `fbuild-serial` yet; the unimplemented ones either delegate out
/// (SWD via pyOCD / probe-rs) or return a typed "caller must do this
/// elsewhere" from [`crate::esp_reset::dispatch_reset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMethod {
    /// esptool's classic-hardware ClassicReset sequence — DTR=low,
    /// RTS=high pulse, RTS=low. Implemented in
    /// [`crate::esp_reset::esp_hard_reset_blocking`].
    DtrRtsPulse,
    /// Single DTR=true→false transition. Drives the Arduino auto-reset
    /// capacitor's edge. **Not yet implemented in fbuild-serial** —
    /// dispatch returns `DelegateToCaller`. Follow-up issue.
    DtrPulse,
    /// Open port at 1200 baud, close → SAMD21/SAMD51 / RP2040 / Teensy
    /// bootloader engages. **Not yet implemented in fbuild-serial** —
    /// dispatch returns `DelegateToCaller`. Follow-up issue.
    TouchBaud1200,
    /// CMSIS-DAP probe via pyOCD / `probe-rs` — out of
    /// `fbuild-serial`'s jurisdiction entirely (the SWD path doesn't
    /// touch the data port). Dispatch always returns
    /// `DelegateToCaller`.
    SwdViaCmsisDap,
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
/// assert_eq!(family_for_vid_pid(0x2341, 0x0043), Some(BoardFamily::ArduinoAutoReset));
/// assert_eq!(family_for_vid_pid(0x2E8A, 0x000A), Some(BoardFamily::NativeUsbCdcReset1200Bps));
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
        // Arduino official — capacitor-coupled DTR auto-reset path
        (0x2341, _) => Some(ArduinoAutoReset),
        // RP2040 — native USB CDC; bootloader entry is a 1200-bps touch
        // (per RP2040 datasheet §USB Controller + pico-sdk's
        // `pico_stdio_usb`). Classify as NativeUsbCdcReset1200Bps so
        // the dispatcher routes to the right primitive instead of the
        // ESP DTR/RTS pulse.
        (0x2E8A, _) => Some(NativeUsbCdcReset1200Bps),
        _ => None,
    }
}

/// Walk `serialport::available_ports()` once and classify the port that
/// matches `name`.
///
/// Returns `None` when the port is not enumerable, is not a USB serial
/// port, or has an unknown VID/PID. Callers that need an idle DTR/RTS
/// state should prefer [`family_for_port_or_default`] so unknown
/// hardware keeps the CDC-ACM host-ready convention.
#[must_use]
pub fn family_for_port(name: &str) -> Option<BoardFamily> {
    let ports = serialport::available_ports().ok()?;
    for port in ports {
        if !serial_port_name_matches(&port.port_name, name) {
            continue;
        }
        if let serialport::SerialPortType::UsbPort(info) = port.port_type {
            return family_for_vid_pid(info.vid, info.pid);
        }
    }
    None
}

/// Classify a serial port, falling back to the safe CDC-ACM host-ready
/// convention when the OS cannot report a known VID/PID.
#[must_use]
pub fn family_for_port_or_default(name: &str) -> BoardFamily {
    family_for_port(name).unwrap_or(BoardFamily::CdcAcmBridge)
}

fn serial_port_name_matches(candidate: &str, requested: &str) -> bool {
    if cfg!(windows) {
        candidate.eq_ignore_ascii_case(requested)
    } else {
        candidate == requested
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
        assert_eq!(BoardFamily::ArduinoAutoReset.idle_dtr_rts(), (true, true));
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
            Some(BoardFamily::ArduinoAutoReset)
        );

        // RP2040 / Pico → native CDC with 1200-bps touch reset (#687)
        assert_eq!(
            family_for_vid_pid(0x2E8A, 0x000A),
            Some(BoardFamily::NativeUsbCdcReset1200Bps)
        );
    }

    #[test]
    fn family_for_vid_pid_returns_none_for_unknown() {
        assert_eq!(family_for_vid_pid(0xDEAD, 0xBEEF), None);
        assert_eq!(family_for_vid_pid(0, 0), None);
    }

    #[test]
    fn family_for_port_default_is_host_ready_cdc() {
        assert_eq!(
            family_for_port_or_default("__fbuild_missing_test_port__"),
            BoardFamily::CdcAcmBridge
        );
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

        // RP2040 / Pico — even though it's NativeUsbCdcReset1200Bps
        // now (#687), idle is still host-ready
        let pico = family_for_vid_pid(0x2E8A, 0x000A).unwrap();
        assert_eq!(pico.idle_dtr_rts(), (true, true));
    }

    // ─── FastLED/fbuild#687: ResetMethod + reset_method() invariants ───

    #[test]
    fn reset_method_maps_each_family_to_its_primitive() {
        use BoardFamily::*;
        use ResetMethod::*;

        assert_eq!(Esp32NativeUsbCdc.reset_method(), DtrRtsPulse);
        assert_eq!(Esp32ExternalUart.reset_method(), DtrRtsPulse);
        assert_eq!(CdcAcmBridge.reset_method(), SwdViaCmsisDap);
        assert_eq!(Teensy.reset_method(), TouchBaud1200);
        assert_eq!(NativeUsbCdcReset1200Bps.reset_method(), TouchBaud1200);
        assert_eq!(ArduinoAutoReset.reset_method(), DtrPulse);
    }

    #[test]
    fn reset_is_serial_native_true_only_for_esp_families() {
        use BoardFamily::*;
        assert!(Esp32NativeUsbCdc.reset_is_serial_native());
        assert!(Esp32ExternalUart.reset_is_serial_native());
        assert!(!CdcAcmBridge.reset_is_serial_native());
        assert!(!Teensy.reset_is_serial_native());
        assert!(!NativeUsbCdcReset1200Bps.reset_is_serial_native());
        assert!(!ArduinoAutoReset.reset_is_serial_native());
    }

    /// FastLED/FastLED#3300 regression guard: every NON-Esp family
    /// MUST end at `(true, true)` idle so a generic open_port that
    /// doesn't know to pulse DTR/RTS still ends at "host ready". If
    /// this test fails after a refactor, the open_port path is
    /// reintroducing the silent-byte-drop bug.
    #[test]
    fn non_esp_families_all_idle_at_host_ready() {
        use BoardFamily::*;
        for family in [
            CdcAcmBridge,
            Teensy,
            NativeUsbCdcReset1200Bps,
            ArduinoAutoReset,
        ] {
            assert_eq!(
                family.idle_dtr_rts(),
                (true, true),
                "family {family:?} must idle at (true, true) — FastLED/FastLED#3300"
            );
        }
    }

    // ─── FastLED/fbuild#691: HandoffTiming per-family table ───────

    #[test]
    fn handoff_timing_matches_fastled_3339_lpc_numbers() {
        // The whole point of #691 is that the LPC845-BRK numbers from
        // FastLED/FastLED#3339 don't drift in three places. Pin them.
        let t = BoardFamily::CdcAcmBridge.handoff_timing();
        assert_eq!(t.post_reset_settle_ms, 500);
        assert_eq!(t.boot_drain_ms, 2000);
        assert_eq!(t.port_reappear_timeout_ms, 3000);
        assert_eq!(t.open_retry_count, 3);
    }

    #[test]
    fn handoff_timing_esp_families_short_settle_no_drain() {
        for family in [
            BoardFamily::Esp32NativeUsbCdc,
            BoardFamily::Esp32ExternalUart,
        ] {
            let t = family.handoff_timing();
            assert_eq!(t.post_reset_settle_ms, 200);
            assert_eq!(t.boot_drain_ms, 0, "ESP CDC peripheral elides boot garbage");
        }
    }

    #[test]
    fn handoff_timing_1200bps_families_tolerate_double_enum() {
        for family in [BoardFamily::Teensy, BoardFamily::NativeUsbCdcReset1200Bps] {
            let t = family.handoff_timing();
            // 1200-bps-touch bootloaders enumerate TWICE — bootloader
            // VID/PID, then app VID/PID after flash. Tolerate both.
            assert!(
                t.port_reappear_timeout_ms >= 3000,
                "family {family:?} needs ≥3 s reappear window"
            );
            assert!(
                t.open_retry_count >= 5,
                "family {family:?} needs ≥5 retries through double-enum"
            );
        }
    }

    #[test]
    fn handoff_timing_arduino_long_settle_no_reappear() {
        let t = BoardFamily::ArduinoAutoReset.handoff_timing();
        assert!(
            t.post_reset_settle_ms >= 1000,
            "Arduino bootloader 'wait for upload' window is ~1.5 s"
        );
        assert_eq!(
            t.port_reappear_timeout_ms, 0,
            "Arduino USB endpoint stays on the bridge chip; no reappear"
        );
    }

    #[test]
    fn new_variants_appear_in_family_for_vid_pid_classification() {
        // RP2040 → NativeUsbCdcReset1200Bps
        assert_eq!(
            family_for_vid_pid(0x2E8A, 0x000A),
            Some(BoardFamily::NativeUsbCdcReset1200Bps)
        );
        // Arduino → ArduinoAutoReset
        assert_eq!(
            family_for_vid_pid(0x2341, 0x0043),
            Some(BoardFamily::ArduinoAutoReset)
        );
        // Teensy shares VID:PID with LPC11U35 — kept as CdcAcmBridge
        // (the more common case in this codebase's deployment
        // ecosystem). Caller wanting Teensy must construct the
        // variant explicitly.
        assert_eq!(
            family_for_vid_pid(0x16C0, 0x0483),
            Some(BoardFamily::CdcAcmBridge)
        );
    }
}
