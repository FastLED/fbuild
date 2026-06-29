//! `fbuild port scan` — enumerate every visible serial port and resolve
//! each VID:PID against the tiered USB resolver
//! ([`fbuild_core::usb::resolve`]).
//!
//! FastLED/fbuild#741. Two rows per port:
//!
//! ```text
//! COM25    303A:1001    USB Serial Device (COM25)    ser=80:F1:B2:…
//!          └─ Espressif Systems / ESP32-S3
//! ```
//!
//! Different from [`super::serial_probe::SerialAction::Probe`]'s `list`
//! action (FastLED/fbuild#686) which annotates from a tiny hardcoded
//! `BOARD_FINGERPRINTS` table — `port scan` consults the full canonical
//! FastLED/boards aggregate via the tiered resolver, so an unrecognized
//! device shows the actual vendor + product name instead of a blank
//! hint.
//!
//! The canonical data source is [FastLED/boards] (see
//! <https://fastled.github.io/boards/> for the live portal). The
//! resolver in `fbuild_core::usb` is wired to consume it via tier-2
//! overlay; that's separate plumbing — this command takes whatever the
//! resolver returns.
//!
//! [FastLED/boards]: https://github.com/FastLED/boards

use clap::Subcommand;
use fbuild_core::{FbuildError, Result};
use std::time::Duration;

use crate::output;

#[derive(Subcommand)]
pub enum PortAction {
    /// Enumerate every visible serial port; for each, render two rows —
    /// the OS-visible identity + a `└─ vendor / product` second row
    /// resolved via [`fbuild_core::usb::resolve`].
    Scan {
        /// Skip the network fetch of the FastLED/boards online overlay
        /// (tier-2 of the resolver). Useful for offline runs — the
        /// embedded vendor archive (tier-1) still provides vendor
        /// names; product columns fall through to the synthetic
        /// `Device 0xPPPP` placeholder.
        #[arg(long)]
        offline: bool,
    },
}

/// Top-level entry — dispatcher calls this.
pub fn run_port(action: PortAction) -> Result<()> {
    match action {
        PortAction::Scan { offline } => run_scan(offline),
    }
}

fn run_scan(offline: bool) -> Result<()> {
    if !offline {
        // Best-effort: populate the tier-2 online overlay so the
        // resolver returns real product names (not just vendor +
        // synthetic placeholder) for VID:PIDs the overlay carries.
        // Errors are swallowed — the resolver always degrades to
        // tier-1 + tier-3 if the overlay can't load.
        populate_online_overlay();
    }
    let ports = serialport::available_ports()
        .map_err(|e| FbuildError::SerialError(format!("serial port enumeration failed: {e}")))?;
    let rendered = render_scan(&ports);
    // render_scan terminates every row with '\n'; strip the trailing newline
    // so result()'s newline doesn't double up.
    output::result(rendered.trim_end_matches('\n'));
    Ok(())
}

/// Fetch the FastLED/boards `usb-vids.proto.zstd` tier-2 overlay backing
/// [`fbuild_core::usb::resolve`] into the local cache root, then install it.
///
/// Best-effort: any I/O / network / parse failure is swallowed and the
/// resolver degrades to tier-1 (embedded vendor archive). The cache is
/// kept fresh on a 7-day cadence — older copies are refetched.
fn populate_online_overlay() {
    let Some(cache_path) = overlay_cache_path() else {
        return;
    };
    if !cache_is_fresh(&cache_path) {
        if let Err(e) = fetch_overlay_to(&cache_path) {
            tracing::debug!(
                error = %e,
                "port scan: overlay fetch failed — degrading to tier-1 only"
            );
        }
    }
    fbuild_core::usb::install_online_cache_proto_zstd(&cache_path);
}

fn overlay_cache_path() -> Option<std::path::PathBuf> {
    let root = fbuild_paths::get_cache_root();
    let dir = root.join("usb");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("usb-vids.proto.zstd"))
}

/// 7-day cache TTL — fbuild's online-data branch refreshes nightly;
/// a weekly local refresh gives us most of the benefit with minimal
/// cold-start network cost. CI / offline boxes still get useful
/// results from the cached copy.
const OVERLAY_TTL_SECS: u64 = 7 * 24 * 60 * 60;

fn cache_is_fresh(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(age) = modified.elapsed() else {
        return false;
    };
    age.as_secs() < OVERLAY_TTL_SECS
}

fn fetch_overlay_to(path: &std::path::Path) -> std::result::Result<(), String> {
    // reqwest::blocking spins its own internal runtime and rejects
    // being called from inside an outer tokio runtime (the CLI
    // dispatcher uses `#[tokio::main]`). Run the fetch on a dedicated
    // OS thread so reqwest's runtime sees a clean async-free context.
    let path = path.to_path_buf();
    std::thread::spawn(move || fetch_overlay_to_inner(&path))
        .join()
        .map_err(|_| "fetch thread panicked".to_string())?
}

fn fetch_overlay_to_inner(path: &std::path::Path) -> std::result::Result<(), String> {
    // FastLED/fbuild#844: route the OS-thread blocking client through the
    // shared bridge so all reqwest construction has one source of truth.
    let client = fbuild_core::http::blocking_client(Duration::from_secs(15));
    fetch_overlay_to_inner_with_client(path, &client, fbuild_core::usb::USB_VIDS_PROTO_ZSTD_URL)
}

fn fetch_overlay_to_inner_with_client(
    path: &std::path::Path,
    client: &reqwest::blocking::Client,
    url: &str,
) -> std::result::Result<(), String> {
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("http get: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("http status {}", response.status()));
    }
    let body = response.bytes().map_err(|e| format!("body read: {e}"))?;
    // Atomic write via a `.tmp` sibling + rename — partial writes from
    // a Ctrl+C mid-fetch don't poison the cache.
    let tmp = path.with_extension("proto.zstd.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("tmp write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    tracing::debug!(
        path = %path.display(),
        size = body.len(),
        "port scan: overlay cache refreshed"
    );
    Ok(())
}

// ─── pure, testable formatter ────────────────────────────────────────

/// Render the entire `fbuild port scan` output for a port list. Pure
/// function so unit tests can pin the layout without standing up a
/// real port enumerator.
///
/// Each port produces two rows + a blank line. The trailing summary
/// line is `N USB ports, M non-USB` for non-empty input, `no serial
/// ports visible\n` for empty.
pub fn render_scan(ports: &[serialport::SerialPortInfo]) -> String {
    if ports.is_empty() {
        return "no serial ports visible\n".to_string();
    }

    let mut out = String::new();
    let mut usb_count = 0usize;
    let mut other_count = 0usize;

    for port in ports {
        match &port.port_type {
            serialport::SerialPortType::UsbPort(info) => {
                usb_count += 1;
                render_usb_port(
                    &mut out,
                    &port.port_name,
                    info.vid,
                    info.pid,
                    info.product.as_deref(),
                    info.serial_number.as_deref(),
                );
            }
            serialport::SerialPortType::PciPort => {
                other_count += 1;
                render_non_usb(&mut out, &port.port_name, "PCI");
            }
            serialport::SerialPortType::BluetoothPort => {
                other_count += 1;
                render_non_usb(&mut out, &port.port_name, "Bluetooth");
            }
            serialport::SerialPortType::Unknown => {
                other_count += 1;
                render_non_usb(&mut out, &port.port_name, "Unknown");
            }
        }
        out.push('\n');
    }

    let usb_plural = if usb_count == 1 { "port" } else { "ports" };
    let non_usb_plural = if other_count == 1 { "port" } else { "ports" };
    use std::fmt::Write as _;
    let _ = writeln!(
        out,
        "{usb_count} USB {usb_plural}, {other_count} non-USB {non_usb_plural}"
    );

    out
}

fn render_usb_port(
    out: &mut String,
    name: &str,
    vid: u16,
    pid: u16,
    product: Option<&str>,
    serial: Option<&str>,
) {
    use std::fmt::Write as _;
    let descriptor = product.unwrap_or("USB Serial Device");
    let serial_field = match serial {
        Some(s) if !s.is_empty() => format!("    ser={s}"),
        _ => String::new(),
    };
    let _ = writeln!(
        out,
        "{name:<10}{vid:04X}:{pid:04X}    {descriptor}{serial_field}",
    );
    let info = fbuild_core::usb::resolve(vid, pid);
    let friendly_product = friendly_product_name(vid, pid, &info.product, product);
    let _ = writeln!(out, "          └─ {} / {}", info.vendor, friendly_product);
}

/// Pick the most "friendly" product label for the resolver row.
///
/// Preference order:
///   1. Resolver's product if it's a real name (tier-2 overlay hit) —
///      i.e. *not* the synthetic `Device 0xPPPP` placeholder.
///   2. Small inline supplement table for common embedded CDC-ACM PIDs
///      that FastLED/boards' canonical `vidpid` table doesn't yet
///      cover (e.g. ESP32-S3 builtin USB-CDC at 303A:1001). Migrate
///      these upstream as the canonical DB picks them up.
///   3. The OS-supplied descriptor when it carries chip-specific
///      detail (e.g. macOS / Linux often expose "CP2102 USB to UART
///      Bridge Controller") — skip if it's the generic "USB Serial
///      Device" Windows fallback.
///   4. Synthetic `Device 0xPPPP` placeholder (tier-3 fallback).
fn friendly_product_name(
    vid: u16,
    pid: u16,
    resolved_product: &str,
    os_descriptor: Option<&str>,
) -> String {
    let synthetic = format!("Device 0x{pid:04X}");
    if resolved_product != synthetic {
        return resolved_product.to_string();
    }
    if let Some(name) = friendly_supplement(vid, pid) {
        return name.to_string();
    }
    if let Some(d) = os_descriptor {
        let trimmed = d.trim();
        if !trimmed.is_empty() && !is_generic_descriptor(trimmed) {
            return trimmed.to_string();
        }
    }
    resolved_product.to_string()
}

/// Strip the trailing `(COMxx)` Windows appends to its USB Serial
/// Device descriptor before testing for generic-ness, so the comparison
/// catches "USB Serial Device (COM25)" too.
fn is_generic_descriptor(d: &str) -> bool {
    let core = d.split('(').next().unwrap_or(d).trim();
    matches!(
        core.to_lowercase().as_str(),
        "usb serial device" | "usb serial port" | "serial usb device" | "usb-serial"
    )
}

/// Small inline supplement for common embedded VID:PIDs that the
/// canonical FastLED/boards `vidpid` table doesn't carry yet. Keep it
/// short — anything that lands upstream should be removed here.
const FRIENDLY_PRODUCTS: &[(u16, u16, &str)] = &[
    // Espressif Systems (VID 0x303A) — ESP32 series USB-CDC ACM.
    (0x303A, 0x1001, "ESP32-S3 USB-CDC"),
    (0x303A, 0x1002, "ESP32-C3 USB-CDC"),
    (0x303A, 0x4001, "ESP32-S2 USB-CDC"),
    (0x303A, 0x0002, "ESP32-S2 ROM-DL"),
    (0x303A, 0x0003, "ESP32-S3 ROM-DL"),
    (0x303A, 0x1000, "ESP32-S2 USB-CDC"),
    // NXP Semiconductors (VID 0x1FC9) — LPC-Link2 / MCU-Link CMSIS-DAP.
    (0x1FC9, 0x0132, "LPC-Link2 CMSIS-DAP"),
    (0x1FC9, 0x0143, "MCU-Link CMSIS-DAP"),
];

fn friendly_supplement(vid: u16, pid: u16) -> Option<&'static str> {
    FRIENDLY_PRODUCTS
        .iter()
        .find(|&&(v, p, _)| v == vid && p == pid)
        .map(|&(_, _, name)| name)
}

fn render_non_usb(out: &mut String, name: &str, kind: &str) {
    use std::fmt::Write as _;
    let _ = writeln!(out, "{name:<10}[{kind}]");
    // Uniform "every port gets two rows" — for non-USB the second row
    // is a placeholder explaining there's no VID:PID to resolve.
    let _ = writeln!(out, "          └─ (no USB identifier — {kind} endpoint)");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    fn usb_port(
        name: &str,
        vid: u16,
        pid: u16,
        product: Option<&str>,
        serial: Option<&str>,
    ) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.to_string(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid,
                serial_number: serial.map(String::from),
                manufacturer: None,
                product: product.map(String::from),
            }),
        }
    }

    #[test]
    fn overlay_cache_path_uses_fbuild_cache_dir() {
        let _env = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvVarGuard::set("FBUILD_CACHE_DIR", tmp.path());

        let path = overlay_cache_path().expect("cache path");

        assert_eq!(path, tmp.path().join("usb").join("usb-vids.proto.zstd"));
        assert!(tmp.path().join("usb").is_dir());
    }

    #[test]
    fn fetch_overlay_writes_cache_file_atomically() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let expected = b"fake proto zstd bytes".to_vec();
        let server_expected = expected.clone();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                server_expected.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(&server_expected).unwrap();
        });

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("usb-vids.proto.zstd");
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        fetch_overlay_to_inner_with_client(&path, &client, &format!("http://{addr}/data"))
            .expect("fetch should write cache");

        handle.join().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert!(!path.with_extension("proto.zstd.tmp").exists());
    }

    #[test]
    fn empty_port_list_renders_canonical_message() {
        assert_eq!(render_scan(&[]), "no serial ports visible\n");
    }

    #[test]
    fn single_usb_port_renders_two_rows_and_summary() {
        let ports = vec![usb_port(
            "COM25",
            0x303A,
            0x1001,
            Some("USB Serial Device (COM25)"),
            Some("80:F1:B2:D1:DF:B1"),
        )];
        let out = render_scan(&ports);
        // Row 1: port + VID:PID + descriptor + serial
        assert!(out.contains("COM25"));
        assert!(out.contains("303A:1001"));
        assert!(out.contains("USB Serial Device (COM25)"));
        assert!(out.contains("ser=80:F1:B2:D1:DF:B1"));
        // Row 2: the `└─` continuation prefix + vendor/product from the
        // tiered resolver. Espressif is in the embedded archive via
        // the inlined supplement.
        assert!(out.contains("└─"));
        assert!(
            out.to_lowercase().contains("espressif"),
            "expected vendor in resolved row, got: {out}"
        );
        // Summary line.
        assert!(out.trim_end().ends_with("1 USB port, 0 non-USB ports"));
    }

    #[test]
    fn unknown_vid_pid_still_gets_the_second_row() {
        // 0xBADD:0xBADD is reserved — the resolver synthesizes the
        // `Unknown vendor` / `Unknown product` placeholder.
        let ports = vec![usb_port("COM99", 0xBADD, 0xBADD, None, None)];
        let out = render_scan(&ports);
        assert!(out.contains("BADD:BADD"));
        assert!(out.contains("└─"));
        assert!(out.contains("Unknown vendor 0xBADD"));
        assert!(out.contains("Unknown product 0xBADD"));
        // Acceptance criterion: every port ALWAYS gets two rows, no
        // exceptions for unrecognized devices.
        let line_count = out.lines().count();
        // Two rows + blank line + summary = 4
        assert_eq!(line_count, 4, "expected 4 lines, got: {out}");
    }

    #[test]
    fn missing_product_descriptor_falls_back_to_default_text() {
        let ports = vec![usb_port("COM7", 0x0403, 0x6001, None, None)];
        let out = render_scan(&ports);
        assert!(out.contains("USB Serial Device"));
        // FTDI VID lives in the embedded archive.
        assert!(
            out.to_lowercase().contains("future technology") || out.to_lowercase().contains("ftdi")
        );
    }

    #[test]
    fn multiple_ports_render_in_order_with_blank_separators() {
        let ports = vec![
            usb_port("COM1", 0x303A, 0x1001, None, None),
            usb_port("COM2", 0x10C4, 0xEA60, None, None),
        ];
        let out = render_scan(&ports);
        // Order preserved.
        let com1_idx = out.find("COM1").unwrap();
        let com2_idx = out.find("COM2").unwrap();
        assert!(com1_idx < com2_idx);
        // Espressif before Silicon Labs (i.e. the resolver row for
        // each port stays grouped with its row-1).
        let esp_idx = out
            .to_lowercase()
            .find("espressif")
            .expect("expected Espressif row");
        let silab_idx = out
            .to_lowercase()
            .find("silicon lab")
            .or_else(|| out.to_lowercase().find("cygnal"))
            .expect("expected Silicon Labs / Cygnal row");
        assert!(esp_idx < silab_idx);
        // Summary.
        assert!(out.trim_end().ends_with("2 USB ports, 0 non-USB ports"));
    }

    #[test]
    fn non_usb_ports_get_kind_label_and_uniform_two_rows() {
        let ports = vec![
            SerialPortInfo {
                port_name: "BT0".to_string(),
                port_type: SerialPortType::BluetoothPort,
            },
            SerialPortInfo {
                port_name: "PCI3".to_string(),
                port_type: SerialPortType::PciPort,
            },
            SerialPortInfo {
                port_name: "X1".to_string(),
                port_type: SerialPortType::Unknown,
            },
        ];
        let out = render_scan(&ports);
        assert!(out.contains("[Bluetooth]"));
        assert!(out.contains("[PCI]"));
        assert!(out.contains("[Unknown]"));
        // Uniform 'every port gets two rows'.
        let arrow_count = out.matches("└─").count();
        assert_eq!(arrow_count, 3);
        // Plural form: 0 USB, 3 non-USB.
        assert!(out.trim_end().ends_with("0 USB ports, 3 non-USB ports"));
    }

    #[test]
    fn summary_singular_form_for_single_port() {
        let ports = vec![usb_port("COM1", 0x303A, 0x1001, None, None)];
        let out = render_scan(&ports);
        // "1 USB port" (singular), not "1 USB ports".
        assert!(out.contains("1 USB port,"));
        assert!(!out.contains("1 USB ports,"));
    }

    #[test]
    fn esp32_s3_cdc_pid_gets_friendly_supplement() {
        // 303A:1001 lacks a product entry in both the embedded archive
        // and the FastLED/boards `vidpid` table; the inline supplement
        // is what makes the row a friendly name instead of synthetic.
        let ports = vec![usb_port(
            "COM25",
            0x303A,
            0x1001,
            Some("USB Serial Device (COM25)"),
            None,
        )];
        let out = render_scan(&ports);
        assert!(
            out.contains("ESP32-S3 USB-CDC"),
            "expected friendly supplement product, got: {out}"
        );
        // And we do NOT fall through to the synthetic placeholder.
        assert!(!out.contains("Device 0x1001"));
    }

    #[test]
    fn specific_os_descriptor_preferred_over_synthetic() {
        // Unknown VID:PID with a non-generic OS descriptor — the
        // descriptor wins over the synthetic placeholder.
        let ports = vec![usb_port(
            "COM7",
            0x303A,
            0xFEED, // Not in the supplement, Espressif VID still
            Some("CP2102 USB to UART Bridge Controller"),
            None,
        )];
        let out = render_scan(&ports);
        assert!(out.contains("CP2102 USB to UART Bridge Controller"));
        assert!(!out.contains("Device 0xFEED"));
    }

    #[test]
    fn generic_windows_descriptor_does_not_override_synthetic() {
        // The bare "USB Serial Device (COM25)" Windows fallback is not
        // chip-specific, so we keep the synthetic placeholder when no
        // supplement applies.
        let ports = vec![usb_port(
            "COM7",
            0x303A,
            0xFEED,
            Some("USB Serial Device (COM7)"),
            None,
        )];
        let out = render_scan(&ports);
        // Find the resolver row (the one with └─) and assert *it*
        // carries the synthetic placeholder, not the generic descriptor.
        let resolver_row = out
            .lines()
            .find(|l| l.contains("└─"))
            .expect("expected a resolver row");
        assert!(
            resolver_row.contains("Device 0xFEED"),
            "expected synthetic placeholder on the resolver row, got: {resolver_row}"
        );
        assert!(
            !resolver_row.contains("USB Serial Device"),
            "generic descriptor leaked into resolver row: {resolver_row}"
        );
    }

    #[test]
    fn mixed_port_list_summary_counts_correctly() {
        let ports = vec![
            usb_port("COM1", 0x303A, 0x1001, None, None),
            SerialPortInfo {
                port_name: "BT0".to_string(),
                port_type: SerialPortType::BluetoothPort,
            },
            usb_port("COM2", 0x16C0, 0x0483, None, None),
        ];
        let out = render_scan(&ports);
        // 2 USB + 1 non-USB. Plural for both since neither is exactly 1.
        assert!(out.trim_end().ends_with("2 USB ports, 1 non-USB port"));
    }
}
