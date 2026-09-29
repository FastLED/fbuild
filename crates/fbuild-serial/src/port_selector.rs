//! `--port ser=<USB serial>`: pick a device by its stable USB serial number
//! instead of an enumeration-order node path (FastLED/fbuild#1428).
//!
//! Node paths (`/dev/ttyACM2`, `COM17`) are assigned by enumeration order, so a
//! device that re-enumerates silently moves, and two boards that swap numbers
//! make a hardcoded path flash the wrong one. A serial selector is resolved
//! against the ports visible *right now*, immediately before use, and never
//! falls back to "the node that used to have it".

use crate::ports::{DetectedPort, available_ports};
use fbuild_core::FbuildError;

/// Prefix that marks a `--port` value as a USB serial selector.
pub const SERIAL_SELECTOR_PREFIX: &str = "ser=";

/// The serial in a `ser=<serial>` selector, or `None` for an ordinary port
/// name (which is passed through untouched).
pub fn serial_selector(port: &str) -> Option<&str> {
    let head = port.get(..SERIAL_SELECTOR_PREFIX.len())?;
    head.eq_ignore_ascii_case(SERIAL_SELECTOR_PREFIX)
        .then(|| &port[SERIAL_SELECTOR_PREFIX.len()..])
}

/// Resolve `serial` to exactly one live port name among `ports`.
///
/// Fails when no selectable device carries the serial (naming it, plus the
/// serials that *are* attached) or when several do (listing every node). A
/// port whose health is known-bad (Windows problem/phantom devnodes) is never
/// selectable, matching the other port pickers.
pub fn resolve_serial(serial: &str, ports: &[DetectedPort]) -> fbuild_core::Result<String> {
    let usb_serial = |port: &DetectedPort| match &port.info.port_type {
        serialport::SerialPortType::UsbPort(usb) => usb.serial_number.clone(),
        _ => None,
    };
    let matches: Vec<&DetectedPort> = ports
        .iter()
        .filter(|port| !port.health.is_known_unhealthy())
        .filter(|port| usb_serial(port).is_some_and(|s| s.eq_ignore_ascii_case(serial)))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.info.port_name.clone()),
        [] => {
            let mut attached: Vec<String> = ports
                .iter()
                .filter_map(|port| {
                    usb_serial(port).map(|s| format!("{} ({})", s, port.info.port_name))
                })
                .collect();
            attached.sort();
            let attached = if attached.is_empty() {
                "none".to_string()
            } else {
                attached.join(", ")
            };
            Err(FbuildError::SerialError(format!(
                "no attached USB serial device has serial '{serial}' (attached USB serials: {attached}); \
                 refusing to fall back to another port"
            )))
        }
        many => {
            let nodes: Vec<&str> = many.iter().map(|p| p.info.port_name.as_str()).collect();
            Err(FbuildError::SerialError(format!(
                "USB serial '{serial}' matches {} ports ({}); pass an explicit port path",
                many.len(),
                nodes.join(", ")
            )))
        }
    }
}

/// Resolve a `--port` argument: a `ser=<serial>` selector becomes the node
/// path of the device carrying that serial (enumerated now); anything else,
/// including `None`, is returned unchanged without touching the port list.
pub fn resolve_port_arg(port: Option<String>) -> fbuild_core::Result<Option<String>> {
    let Some(serial) = port.as_deref().and_then(serial_selector) else {
        return Ok(port);
    };
    let ports = available_ports()
        .map_err(|e| FbuildError::SerialError(format!("failed to enumerate serial ports: {e}")))?;
    resolve_serial(serial, &ports).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::PortHealth;

    fn usb(name: &str, serial: Option<&str>, health: PortHealth) -> DetectedPort {
        let mut port = DetectedPort::unknown(serialport::SerialPortInfo {
            port_name: name.to_string(),
            port_type: serialport::SerialPortType::UsbPort(serialport::UsbPortInfo {
                vid: 0x303a,
                pid: 0x1001,
                serial_number: serial.map(str::to_string),
                manufacturer: None,
                product: None,
                interface: None,
            }),
        });
        port.health = health;
        port
    }

    #[test]
    fn only_ser_prefixed_values_are_selectors() {
        assert_eq!(serial_selector("ser=ABC123"), Some("ABC123"));
        assert_eq!(
            serial_selector("SER=8C:BF:EA:CF:87:B4"),
            Some("8C:BF:EA:CF:87:B4")
        );
        assert_eq!(serial_selector("/dev/ttyACM2"), None);
        assert_eq!(serial_selector("COM17"), None);
        assert_eq!(serial_selector("se"), None);
    }

    #[test]
    fn resolves_to_the_node_currently_carrying_the_serial() {
        let ports = [
            usb(
                "/dev/ttyACM0",
                Some("2DCB876B587EA334"),
                PortHealth::Unknown,
            ),
            usb(
                "/dev/ttyACM1",
                Some("8C:BF:EA:CF:87:B4"),
                PortHealth::Unknown,
            ),
        ];
        assert_eq!(
            resolve_serial("8c:bf:ea:cf:87:b4", &ports).unwrap(),
            "/dev/ttyACM1"
        );
        // The same board after it re-enumerates as another node.
        let moved = [usb(
            "/dev/ttyACM2",
            Some("8C:BF:EA:CF:87:B4"),
            PortHealth::Unknown,
        )];
        assert_eq!(
            resolve_serial("8C:BF:EA:CF:87:B4", &moved).unwrap(),
            "/dev/ttyACM2"
        );
    }

    #[test]
    fn missing_serial_fails_naming_it_and_the_attached_ones() {
        let ports = [usb("/dev/ttyACM0", Some("AAAA"), PortHealth::Unknown)];
        let err = resolve_serial("BBBB", &ports).unwrap_err().to_string();
        assert!(err.contains("BBBB"), "{err}");
        assert!(err.contains("AAAA (/dev/ttyACM0)"), "{err}");
        assert!(err.contains("refusing to fall back"), "{err}");
    }

    #[test]
    fn ambiguous_serial_fails_listing_every_node() {
        let ports = [
            usb("/dev/ttyACM0", Some("DUP"), PortHealth::Unknown),
            usb("/dev/ttyACM1", Some("DUP"), PortHealth::Unknown),
        ];
        let err = resolve_serial("DUP", &ports).unwrap_err().to_string();
        assert!(
            err.contains("/dev/ttyACM0") && err.contains("/dev/ttyACM1"),
            "{err}"
        );
    }

    #[test]
    fn known_unhealthy_and_non_usb_ports_are_never_selected() {
        let phantom = PortHealth::Phantom {
            problem_code: None,
            status: None,
        };
        let ports = [
            usb("COM9", Some("GHOST"), phantom),
            usb("COM3", None, PortHealth::Unknown),
        ];
        assert!(resolve_serial("GHOST", &ports).is_err());
    }

    #[test]
    fn plain_port_arguments_pass_through_without_enumeration() {
        assert_eq!(resolve_port_arg(None).unwrap(), None);
        assert_eq!(
            resolve_port_arg(Some("/dev/ttyACM2".to_string())).unwrap(),
            Some("/dev/ttyACM2".to_string())
        );
    }
}
