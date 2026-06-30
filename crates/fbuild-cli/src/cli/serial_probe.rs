//! `fbuild serial probe …` — port-introspection / debug helper.
//!
//! Mirror of FastLED's `ci/util/serial_probe.py` (FastLED/FastLED#3339),
//! filed against fbuild as FastLED/fbuild#686. Three actions today:
//!
//! - `list` — enumerate every serial port `serialport` can see and
//!   annotate each with the [`fbuild_serial::boards::board_hint`] for
//!   its `(vid, pid)`. The annotation makes "is COM12 the LPC845-BRK's
//!   VCOM bridge or the CMSIS-DAP debug probe?" answerable at a glance
//!   without firing up Device Manager or `lsusb -v`.
//!
//! - `find` — return ONE matching port (exit 0) or no match (exit 1)
//!   by either a literal `--vid-pid V:P` or a PlatformIO-style
//!   `--env <name>` that's mapped via
//!   [`fbuild_serial::boards::vcom_for_env`]. The env path is what
//!   FastLED's [`port_utils.py`] uses to pick the VCOM bridge port
//!   for an LPC845-BRK (which enumerates TWO USB devices — CMSIS-DAP
//!   debug AND the LPC11U35 VCOM — and only the second is the data
//!   port).
//!
//! - `read` — open a port with the **correct** DTR/RTS for the
//!   inferred board family per
//!   [`fbuild_serial::boards::BoardFamily::idle_dtr_rts`], optionally
//!   send a payload, then read bytes for a bounded duration and dump
//!   them to stdout. The DTR/RTS-aware open is the whole point: the
//!   FastLED/FastLED#3300 "silence on LPC845-BRK" failure was caused
//!   by exactly this kind of helper ignoring DTR and leaving the
//!   bridge in "host not ready" — every byte the target MCU
//!   transmitted got silently dropped.
//!
//! [`port_utils.py`]: https://github.com/FastLED/FastLED/blob/master/ci/util/port_utils.py

use std::io::Read as _;
use std::time::{Duration, Instant};

use clap::Subcommand;
use fbuild_core::{FbuildError, Result};
use fbuild_serial::boards::{board_hint, family_for_port_or_default, vcom_for_env};

use crate::output;

#[derive(Subcommand)]
pub enum SerialAction {
    /// Port-probe utilities — list, find, read.
    Probe {
        #[command(subcommand)]
        action: ProbeAction,
    },
}

#[derive(Subcommand)]
pub enum ProbeAction {
    /// List every visible serial port with VID:PID + board hint.
    List,
    /// Look up a port by VID:PID or by PlatformIO env name. Exits 0
    /// with the device path on stdout when a match is found, exit 1
    /// when no match.
    Find {
        /// Filter by literal hex VID:PID, e.g. `--vid-pid 16C0:0483`.
        /// Hex digits, separated by `:` or `,`. Case-insensitive.
        #[arg(long = "vid-pid")]
        vid_pid: Option<String>,
        /// Filter by PlatformIO env name (`lpc845brk`, `lpc804`, …)
        /// — looks up the VCOM `(vid, pid)` in
        /// [`fbuild_serial::boards::ENVIRONMENT_TO_VCOM`] and matches
        /// that against the connected ports.
        #[arg(long)]
        env: Option<String>,
    },
    /// Open a port, optionally send a payload, then read bytes to
    /// stdout for up to `--seconds`. DTR/RTS is picked from the
    /// inferred [`BoardFamily`] so CDC-ACM bridges (LPC11U35, FTDI
    /// CDC) see "host ready" rather than the ESP-default
    /// "host not ready" that drops bytes silently.
    Read {
        /// Device path (e.g. `COM12`, `/dev/ttyUSB0`).
        port: String,
        /// Baud rate. Default 115200 — matches the FastLED bring-up
        /// default and esp-idf's USB CDC.
        #[arg(long, default_value_t = 115_200)]
        baud: u32,
        /// Read window in seconds.
        #[arg(long, default_value_t = 4.0)]
        seconds: f64,
        /// Bytes to write to the port before starting the read window.
        /// Useful for jsonrpc echo probes:
        /// `--send '{"jsonrpc":"2.0","id":1,"method":"echo","params":[1]}\n'`
        #[arg(long)]
        send: Option<String>,
    },
}

/// Top-level entry — dispatcher calls this.
pub fn run_serial(action: SerialAction) -> Result<()> {
    match action {
        SerialAction::Probe { action } => run_probe(action),
    }
}

fn run_probe(action: ProbeAction) -> Result<()> {
    match action {
        ProbeAction::List => list_ports(),
        ProbeAction::Find { vid_pid, env } => find_port(vid_pid.as_deref(), env.as_deref()),
        ProbeAction::Read {
            port,
            baud,
            seconds,
            send,
        } => read_port(&port, baud, seconds, send.as_deref()),
    }
}

/// `fbuild serial probe list` — enumerate every port with annotation.
fn list_ports() -> Result<()> {
    let ports = serialport::available_ports()
        .map_err(|e| FbuildError::SerialError(format!("serial port enumeration failed: {e}")))?;

    if ports.is_empty() {
        output::warn("no serial ports visible");
        return Ok(());
    }

    for port in &ports {
        print_port_summary(port);
    }
    Ok(())
}

fn print_port_summary(port: &serialport::SerialPortInfo) {
    let name = &port.port_name;
    match &port.port_type {
        serialport::SerialPortType::UsbPort(info) => {
            let hint = board_hint(info.vid, info.pid)
                .map(|h| format!("[{h}]"))
                .unwrap_or_default();
            let product = info.product.as_deref().unwrap_or("");
            let serial = info.serial_number.as_deref().unwrap_or("");
            let serial_field = if serial.is_empty() {
                String::new()
            } else {
                format!("ser={serial}  ")
            };
            output::result(format!(
                "{name:<10} {vid:04X}:{pid:04X}  {serial_field}{product}  {hint}",
                vid = info.vid,
                pid = info.pid,
            ));
        }
        serialport::SerialPortType::PciPort => output::result(format!("{name:<10} [PCI]")),
        serialport::SerialPortType::BluetoothPort => {
            output::result(format!("{name:<10} [Bluetooth]"))
        }
        serialport::SerialPortType::Unknown => output::result(format!("{name:<10} [Unknown]")),
    }
}

/// `fbuild serial probe find …` — single-port lookup. Exit 0 with the
/// matched device path on stdout, exit 1 when nothing matches. Errors
/// (bad CLI args, port enumeration failure) propagate as `Result::Err`.
fn find_port(vid_pid: Option<&str>, env: Option<&str>) -> Result<()> {
    let target = match (vid_pid, env) {
        (Some(_), Some(_)) => {
            return Err(FbuildError::SerialError(
                "`--vid-pid` and `--env` are mutually exclusive".to_string(),
            ));
        }
        (Some(s), None) => parse_vid_pid(s)?,
        (None, Some(env_name)) => vcom_for_env(env_name).ok_or_else(|| {
            FbuildError::SerialError(format!(
                "no VCOM mapping for env `{env_name}` — \
                 add to ENVIRONMENT_TO_VCOM in fbuild-serial::boards"
            ))
        })?,
        (None, None) => {
            return Err(FbuildError::SerialError(
                "must supply one of `--vid-pid` or `--env`".to_string(),
            ));
        }
    };

    let ports = serialport::available_ports()
        .map_err(|e| FbuildError::SerialError(format!("serial port enumeration failed: {e}")))?;

    for port in ports {
        if let serialport::SerialPortType::UsbPort(info) = &port.port_type {
            if (info.vid, info.pid) == target {
                output::result(&port.port_name);
                return Ok(());
            }
        }
    }
    // No match — non-zero exit so scripting can branch.
    std::process::exit(1);
}

/// Parse `"VID:PID"` / `"VID,PID"` (hex, case-insensitive) into a
/// `(u16, u16)`. Whitespace stripped.
fn parse_vid_pid(s: &str) -> Result<(u16, u16)> {
    let raw = s.trim();
    let (vid_s, pid_s) = raw
        .split_once(':')
        .or_else(|| raw.split_once(','))
        .ok_or_else(|| {
            FbuildError::SerialError(format!(
                "expected `VID:PID` (e.g. `16C0:0483`), got `{raw}`"
            ))
        })?;
    let vid = u16::from_str_radix(vid_s.trim(), 16)
        .map_err(|e| FbuildError::SerialError(format!("bad VID hex `{vid_s}`: {e}")))?;
    let pid = u16::from_str_radix(pid_s.trim(), 16)
        .map_err(|e| FbuildError::SerialError(format!("bad PID hex `{pid_s}`: {e}")))?;
    Ok((vid, pid))
}

/// `fbuild serial probe read PORT …` — open with correct DTR/RTS,
/// optionally send a payload, then read bytes to stdout for the
/// duration. Uses `family_for_vid_pid` against the resolved port to
/// pick a [`BoardFamily`] and consults
/// [`BoardFamily::idle_dtr_rts`] for the open-time state.
fn read_port(port_name: &str, baud: u32, seconds: f64, send: Option<&str>) -> Result<()> {
    // Resolve the family by looking up the connected port's VID:PID,
    // if `serialport` can see it. Unknown VID:PID → safe-default
    // `CdcAcmBridge` (DTR=true, RTS=true) per the FastLED/FastLED#3300
    // bug mode — better to over-assert than to silently drop.
    let family = family_for_port_or_default(port_name);
    let (dtr, rts) = family.idle_dtr_rts();

    let mut handle = serialport::new(port_name, baud)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| FbuildError::SerialError(format!("open `{port_name}` @ {baud}: {e}")))?;
    handle
        .write_data_terminal_ready(dtr)
        .map_err(|e| FbuildError::SerialError(format!("set DTR={dtr} on `{port_name}`: {e}")))?;
    handle
        .write_request_to_send(rts)
        .map_err(|e| FbuildError::SerialError(format!("set RTS={rts} on `{port_name}`: {e}")))?;

    output::progress(format!(
        "probe read: port={port_name} baud={baud} family={family:?} \
         dtr={dtr} rts={rts} seconds={seconds}"
    ));

    if let Some(payload) = send {
        let bytes = unescape_send(payload);
        std::io::Write::write_all(&mut *handle, &bytes)
            .map_err(|e| FbuildError::SerialError(format!("send to `{port_name}`: {e}")))?;
    }

    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut buf = [0u8; 1024];
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    while Instant::now() < deadline {
        match handle.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => std::io::Write::write_all(&mut stdout, &buf[..n])
                .map_err(|e| FbuildError::SerialError(format!("stdout write: {e}")))?,
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                return Err(FbuildError::SerialError(format!(
                    "read from `{port_name}`: {e}"
                )));
            }
        }
    }
    std::io::Write::flush(&mut stdout).ok();
    Ok(())
}

/// Interpret a handful of backslash escapes in `--send` payloads so
/// scripted probes (`--send 'hello\n'`, `--send '\x7e'`) work without
/// shell quoting gymnastics. Anything we don't recognize passes
/// through verbatim.
fn unescape_send(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            // ASCII fast path; higher code points get encoded as UTF-8.
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('0') => out.push(0),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let h1 = chars.next();
                let h2 = chars.next();
                if let (Some(a), Some(b)) = (h1, h2) {
                    if let Ok(byte) = u8::from_str_radix(&format!("{a}{b}"), 16) {
                        out.push(byte);
                        continue;
                    }
                }
                // Malformed `\xZZ` — pass through verbatim.
                out.push(b'\\');
                out.push(b'x');
                if let Some(a) = h1 {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(a.encode_utf8(&mut buf).as_bytes());
                }
                if let Some(b) = h2 {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(b.encode_utf8(&mut buf).as_bytes());
                }
            }
            Some(other) => {
                out.push(b'\\');
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vid_pid_accepts_colon_and_comma() {
        assert_eq!(parse_vid_pid("16C0:0483").unwrap(), (0x16C0, 0x0483));
        assert_eq!(parse_vid_pid("16c0,0483").unwrap(), (0x16C0, 0x0483));
        assert_eq!(parse_vid_pid(" 303A : 1001 ").unwrap(), (0x303A, 0x1001));
    }

    #[test]
    fn parse_vid_pid_rejects_malformed() {
        assert!(parse_vid_pid("16C0").is_err());
        assert!(parse_vid_pid("16C00483").is_err());
        assert!(parse_vid_pid("GG:0000").is_err());
        assert!(parse_vid_pid("").is_err());
    }

    #[test]
    fn unescape_send_recognizes_common_escapes() {
        assert_eq!(unescape_send("hello\\n"), b"hello\n");
        assert_eq!(unescape_send("a\\rb"), b"a\rb");
        assert_eq!(unescape_send("x\\ty"), b"x\ty");
        assert_eq!(unescape_send("\\\\"), b"\\");
        assert_eq!(unescape_send("ab\\0cd"), b"ab\0cd");
    }

    #[test]
    fn unescape_send_decodes_hex_byte() {
        assert_eq!(unescape_send("\\x7e"), &[0x7e]);
        assert_eq!(unescape_send("\\x00"), &[0]);
        assert_eq!(unescape_send("\\xFF"), &[0xFF]);
        assert_eq!(unescape_send("pre\\x41post"), b"preApost");
    }

    #[test]
    fn unescape_send_passes_unknown_escape_verbatim() {
        // Don't surprise the user with silent byte mangling on a
        // sequence we don't recognize.
        assert_eq!(unescape_send("\\q"), b"\\q");
        assert_eq!(unescape_send("\\xZZ"), b"\\xZZ");
    }

    #[test]
    fn unescape_send_handles_trailing_backslash() {
        // Edge case: trailing `\` with no following char — pass through.
        assert_eq!(unescape_send("foo\\"), b"foo\\");
    }
}
