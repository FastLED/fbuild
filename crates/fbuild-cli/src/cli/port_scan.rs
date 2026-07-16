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
//! The resolver consumes the published FastLED/boards display-name artifact.
//! Unknown devices receive deterministic fallback labels rather than a copied
//! catalogue in fbuild.

use clap::Subcommand;
use fbuild_core::{FbuildError, Result};

use crate::output;

#[derive(Subcommand)]
pub enum PortAction {
    /// Enumerate every visible serial port; for each, render two rows —
    /// the OS-visible identity + a `└─ vendor / product` second row
    /// resolved via [`fbuild_core::usb::resolve`].
    Scan {
        /// Skip refreshing the FastLED/boards display-name cache. Useful for
        /// offline runs; uncached identities receive `Unknown` labels.
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
        // Best-effort: refresh the FastLED/boards display-name cache. Errors
        // are swallowed because enumeration can still render unknown labels.
        populate_online_overlay();
    }
    // Use fbuild-serial's blessed enumerator, not `serialport::available_ports()`
    // directly: on Windows the latter drops every port whose PnP devnode reports
    // a non-OK status (all PJRC/Teensy composite ports). FastLED/fbuild#962.
    let ports = fbuild_serial::ports::available_ports()
        .map_err(|e| FbuildError::SerialError(format!("serial port enumeration failed: {e}")))?;
    let rendered = render_scan(&ports);
    // render_scan terminates every row with '\n'; strip the trailing newline
    // so result()'s newline doesn't double up.
    output::result(rendered.trim_end_matches('\n'));
    let problem_devices = fbuild_serial::ports::present_usb_problem_devices();
    if !problem_devices.is_empty() {
        // This is actionable final command output. Keep it visible under the
        // default tracing filter instead of requiring users to set RUST_LOG.
        output::diagnostic(format!(
            "warning: {}",
            format_usb_problem_warning(&problem_devices)
        ));
    }
    Ok(())
}

fn format_usb_problem_warning(devices: &[fbuild_serial::ports::UsbProblemDevice]) -> String {
    use std::fmt::Write as _;
    let mut warning = String::from(
        "Windows reports present USB device(s) with hardware problems; fbuild cannot associate these unidentified nodes with a target board:",
    );
    let mut hub_seen = false;
    for device in devices {
        let location = device
            .location
            .as_deref()
            .map(|value| format!(" at {value}"))
            .unwrap_or_default();
        let topology = match device.behind_external_hub {
            Some(true) => {
                hub_seen = true;
                " behind an external USB hub"
            }
            Some(false) => " on a direct root USB port",
            None => " (USB topology unavailable)",
        };
        let name = device
            .friendly_name
            .as_deref()
            .unwrap_or("Unknown USB device");
        let _ = write!(
            warning,
            "\n  - {name}: Windows problem code {}{location}{topology}",
            device.problem_code
        );
    }
    if hub_seen {
        warning.push_str(
            "\nRecommendation: connect platform USB devices directly to a motherboard USB port; external hubs can introduce power/reset/timing conditions that disrupt USB enumeration.",
        );
    }
    warning
}

/// Fetch the FastLED/boards display-name artifact backing
/// [`fbuild_core::usb::resolve`] into the local cache root, then install it.
///
/// Best-effort: any I/O / network / parse failure is swallowed and the
/// resolver degrades to deterministic unknown labels. The cache is kept fresh
/// on a 7-day cadence — older copies are refetched.
fn populate_online_overlay() {
    populate_online_overlay_from_urls(
        fbuild_core::usb::USB_VIDS_PROTO_ZSTD_URL,
        fbuild_core::usb::USB_VID_JSON_URL,
    );
}

fn populate_online_overlay_from_urls(proto_url: &str, json_url: &str) {
    let root = fbuild_paths::get_cache_root();
    populate_online_overlay_from_urls_in(proto_url, json_url, &root);
}

fn populate_online_overlay_from_urls_in(proto_url: &str, json_url: &str, root: &std::path::Path) {
    let Some(proto_cache_path) = overlay_cache_path_in(root) else {
        return;
    };
    let Some(json_cache_path) = overlay_json_cache_path_in(root) else {
        return;
    };
    if !fbuild_core::usb::populate_online_cache_from_paths_and_urls(
        &proto_cache_path,
        &json_cache_path,
        proto_url,
        json_url,
    ) {
        tracing::debug!("port scan: FastLED/boards display-name cache unavailable");
    }
}

fn overlay_cache_path_in(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = root.join("usb");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("usb-vids.proto.zstd"))
}

fn overlay_json_cache_path_in(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = root.join("usb");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("usb-vid.json"))
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
                    info.interface,
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
    interface: Option<u8>,
) {
    let kernel_class = fbuild_serial::port_class::detect_port_kernel_class(name);
    render_usb_port_with_kernel_class(
        out,
        name,
        vid,
        pid,
        product,
        serial,
        interface,
        kernel_class,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_usb_port_with_kernel_class(
    out: &mut String,
    name: &str,
    vid: u16,
    pid: u16,
    product: Option<&str>,
    serial: Option<&str>,
    interface: Option<u8>,
    kernel_class: Option<fbuild_serial::port_class::PortKernelClass>,
) {
    use std::fmt::Write as _;
    let descriptor = product.unwrap_or("USB Serial Device");
    let serial_field = match serial {
        Some(s) if !s.is_empty() => format!("    ser={s}"),
        _ => String::new(),
    };
    // Surface the composite-interface index so multi-function devices (a
    // Teensy exposing Serial + MIDI, say) make the data-bearing COM port
    // obvious. FastLED/fbuild#962.
    let interface_field = match interface {
        Some(n) => format!("    if=MI_{n:02}"),
        None => String::new(),
    };
    let _ = writeln!(
        out,
        "{name:<10}{vid:04X}:{pid:04X}    {descriptor}{serial_field}{interface_field}",
    );
    let info = fbuild_core::usb::resolve(vid, pid);
    let friendly_product = friendly_product_name(pid, &info.product, product);
    let _ = writeln!(
        out,
        "          └─ {} / {}    cdc={}",
        info.vendor,
        friendly_product,
        cdc_label(kernel_class)
    );
}

fn cdc_label(kernel_class: Option<fbuild_serial::port_class::PortKernelClass>) -> &'static str {
    match kernel_class {
        Some(fbuild_serial::port_class::PortKernelClass::CdcAcm) => "yes",
        Some(fbuild_serial::port_class::PortKernelClass::UsbSerialBridge) => "no",
        None => "unknown",
    }
}

/// Pick the most "friendly" product label for the resolver row.
///
/// Preference order:
///   1. Resolver's product if it is a real FastLED/boards name rather than a
///      deterministic `Device` or `Unknown product` placeholder.
///   2. The OS-supplied descriptor when it carries chip-specific detail
///      (e.g. macOS / Linux often expose "CP2102 USB to UART Bridge
///      Controller") — skip if it's the generic "USB Serial Device"
///      Windows fallback.
///   3. Synthetic `Device 0xPPPP` placeholder.
///
/// There is intentionally NO hardcoded per-PID product table here: friendly
/// product names are owned by FastLED/boards and fetched at runtime
/// (FastLED/fbuild#722, #959). A missing name is a data gap to fix there, not
/// in fbuild source.
fn friendly_product_name(pid: u16, resolved_product: &str, os_descriptor: Option<&str>) -> String {
    let synthetic = format!("Device 0x{pid:04X}");
    let unknown = format!("Unknown product 0x{pid:04X}");
    if resolved_product != synthetic && resolved_product != unknown {
        return resolved_product.to_string();
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
    use std::sync::{Mutex, MutexGuard};
    use std::time::Duration;

    static USB_CACHE_LOCK: Mutex<()> = Mutex::new(());

    fn install_name_fixture() -> MutexGuard<'static, ()> {
        let guard = USB_CACHE_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("usb-ids.json");
        std::fs::write(
            &path,
            r#"{
                "303a":{"vendor":"Espressif Systems","products":[["1001","USB JTAG/serial debug unit"]]},
                "0403":{"vendor":"Future Technology Devices International","products":[["6001","FT232 Serial UART"]]},
                "10c4":{"vendor":"Silicon Labs","products":[["ea60","CP210x UART Bridge"]]},
                "16c0":{"vendor":"PJRC","products":[["0483","Teensy USB Serial"]]}
            }"#,
        )
        .unwrap();
        assert!(fbuild_core::usb::try_install_online_cache(&path));
        guard
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
                interface: None,
            }),
        }
    }

    #[test]
    fn overlay_cache_paths_use_supplied_cache_root() {
        let tmp = tempfile::tempdir().unwrap();

        let path = overlay_cache_path_in(tmp.path()).expect("cache path");
        let json_path = overlay_json_cache_path_in(tmp.path()).expect("json cache path");

        assert_eq!(path, tmp.path().join("usb").join("usb-vids.proto.zstd"));
        assert_eq!(json_path, tmp.path().join("usb").join("usb-vid.json"));
        assert!(tmp.path().join("usb").is_dir());
    }

    #[test]
    fn populate_overlay_falls_back_to_json_when_proto_is_missing() {
        let _usb = USB_CACHE_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let json = r#"{"feed":{"vendor":"Feedface Inc","products":[["c0de","Coded Widget"]]}}"#;
        let server_json = json.as_bytes().to_vec();
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut request_count = 0_u8;
            while request_count < 2 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0_u8; 1024];
                        let _ = stream.read(&mut request).unwrap();
                        request_count += 1;
                        if request_count == 1 {
                            stream
                                .write_all(
                                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                )
                                .unwrap();
                        } else {
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                server_json.len()
                            );
                            stream.write_all(response.as_bytes()).unwrap();
                            stream.write_all(&server_json).unwrap();
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "timed out waiting for overlay requests"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("accept failed: {e}"),
                }
            }
            request_count
        });

        populate_online_overlay_from_urls_in(
            &format!("http://{addr}/usb-vids.proto.zstd"),
            &format!("http://{addr}/usb-vid.json"),
            tmp.path(),
        );

        assert_eq!(handle.join().unwrap(), 2);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("usb").join("usb-vid.json")).unwrap(),
            json
        );
        let info = fbuild_core::usb::resolve(0xFEED, 0xC0DE);
        assert_eq!(info.vendor, "Feedface Inc");
        assert_eq!(info.product, "Coded Widget");
    }

    #[test]
    fn empty_port_list_renders_canonical_message() {
        assert_eq!(render_scan(&[]), "no serial ports visible\n");
    }

    #[test]
    fn single_usb_port_renders_two_rows_and_summary() {
        let _usb = install_name_fixture();
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
        // explicitly installed runtime fixture.
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
        let _usb = install_name_fixture();
        let ports = vec![usb_port("COM7", 0x0403, 0x6001, None, None)];
        let out = render_scan(&ports);
        assert!(out.contains("USB Serial Device"));
        // The explicit runtime fixture supplies the FTDI name.
        assert!(
            out.to_lowercase().contains("future technology") || out.to_lowercase().contains("ftdi")
        );
    }

    #[test]
    fn multiple_ports_render_in_order_with_blank_separators() {
        let _usb = install_name_fixture();
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
    fn usb_port_rows_show_cdc_classification() {
        use fbuild_serial::port_class::PortKernelClass;

        let mut cdc = String::new();
        render_usb_port_with_kernel_class(
            &mut cdc,
            "COM1",
            0x303A,
            0x1001,
            None,
            None,
            None,
            Some(PortKernelClass::CdcAcm),
        );
        assert!(cdc.contains("cdc=yes"), "got: {cdc}");

        let mut bridge = String::new();
        render_usb_port_with_kernel_class(
            &mut bridge,
            "COM2",
            0x10C4,
            0xEA60,
            None,
            None,
            None,
            Some(PortKernelClass::UsbSerialBridge),
        );
        assert!(bridge.contains("cdc=no"), "got: {bridge}");

        let mut unknown = String::new();
        render_usb_port_with_kernel_class(
            &mut unknown,
            "COM3",
            0x303A,
            0x1001,
            None,
            None,
            None,
            None,
        );
        assert!(unknown.contains("cdc=unknown"), "got: {unknown}");
    }

    #[test]
    fn usb_problem_warning_recommends_root_port_for_hub_node() {
        let devices = vec![fbuild_serial::ports::UsbProblemDevice {
            instance_id: r"USB\VID_0000&PID_0002\failure".to_string(),
            problem_code: 43,
            friendly_name: Some(
                "Unknown USB Device (Device Descriptor Request Failed)".to_string(),
            ),
            location: Some("Port_#0001.Hub_#0011".to_string()),
            behind_external_hub: Some(true),
        }];
        let warning = format_usb_problem_warning(&devices);
        assert!(warning.contains("problem code 43"));
        assert!(warning.contains("behind an external USB hub"));
        assert!(warning.contains("Port_#0001.Hub_#0011"));
        assert!(warning.contains("connect platform USB devices directly"));
        assert!(warning.contains("cannot associate"));
    }

    #[test]
    fn usb_problem_warning_does_not_overclaim_direct_root_node() {
        let devices = vec![fbuild_serial::ports::UsbProblemDevice {
            instance_id: r"USB\VID_0000&PID_0002\failure".to_string(),
            problem_code: 43,
            friendly_name: None,
            location: Some("Port_#0014.Hub_#0001".to_string()),
            behind_external_hub: Some(false),
        }];
        let warning = format_usb_problem_warning(&devices);
        assert!(warning.contains("Unknown USB device"));
        assert!(warning.contains("on a direct root USB port"));
        assert!(!warning.contains("connect platform USB devices directly"));
    }

    #[test]
    fn teensy_16c0_port_resolves_pjrc_from_runtime_fixture() {
        let _usb = install_name_fixture();
        // The enumeration fix (fbuild-serial's Windows walk) is what makes a
        // Teensy show up at all; here we pin that once it IS enumerated, the
        // scan resolves VID 16C0 to PJRC. The vendor + product names come from
        // an explicit runtime-cache fixture, not production constants.
        // FastLED/fbuild#962.
        let ports = vec![usb_port(
            "COM20",
            0x16C0,
            0x0483,
            Some("USB Serial Device (COM20)"),
            None,
        )];
        let out = render_scan(&ports);
        assert!(out.contains("16C0:0483"), "got: {out}");
        assert!(
            out.to_lowercase().contains("pjrc") || out.to_lowercase().contains("teensy"),
            "expected PJRC/Teensy from the runtime fixture, got: {out}"
        );
    }

    #[test]
    fn composite_interface_index_is_surfaced() {
        // MI_xx bonus: a Teensy Serial+MIDI composite exposes its interface
        // index so the data-bearing COM port is obvious.
        let mut out = String::new();
        render_usb_port(&mut out, "COM7", 0x16C0, 0x0489, None, None, Some(0));
        assert!(out.contains("if=MI_00"), "expected MI index, got: {out}");
    }

    #[test]
    fn common_esp32_cdc_pid_resolves_from_runtime_fixture() {
        let _usb = install_name_fixture();
        // The common ESP32 USB-Serial-JTAG PID (303A:1001) resolves to a real
        // product name from an explicit runtime-cache fixture, not a production
        // fallback table. FastLED/fbuild#722.
        let ports = vec![usb_port(
            "COM25",
            0x303A,
            0x1001,
            Some("USB Serial Device (COM25)"),
            None,
        )];
        let out = render_scan(&ports);
        assert!(
            out.to_lowercase().contains("espressif"),
            "expected Espressif vendor from the runtime fixture, got: {out}"
        );
        // A real archive product name, not the synthetic placeholder or the
        // generic Windows descriptor.
        assert!(!out.contains("Device 0x1001"), "leaked placeholder: {out}");
        let resolver_row = out
            .lines()
            .find(|l| l.contains("└─"))
            .expect("resolver row");
        assert!(
            !resolver_row.contains("USB Serial Device"),
            "generic descriptor leaked into resolver row: {resolver_row}"
        );
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
        // chip-specific, so we keep the deterministic unknown placeholder.
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
            resolver_row.contains("Unknown product 0xFEED"),
            "expected unknown placeholder on the resolver row, got: {resolver_row}"
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
