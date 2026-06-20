//! Tiered USB VID:PID → name resolver. See the [crate::usb] module-level
//! documentation for the design.

use serde::{Deserialize, Serialize};

/// Resolved USB device identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbInfo {
    pub vendor: String,
    pub product: String,
}

/// Best-effort lookup. Never returns `None`: a synthetic
/// `"Unknown vendor 0xVVVV"` / `"Unknown product 0xPPPP"` is produced
/// when both tier-1 (bundled) and tier-2 (online overlay) miss.
pub fn resolve(vid: u16, pid: u16) -> UsbInfo {
    try_resolve(vid, pid).unwrap_or_else(|| UsbInfo {
        vendor: format!("Unknown vendor 0x{vid:04X}"),
        product: format!("Unknown product 0x{pid:04X}"),
    })
}

/// Tier-1 + tier-2 only. Returns `None` if neither knows this pair.
pub fn try_resolve(vid: u16, pid: u16) -> Option<UsbInfo> {
    resolve_bundled(vid, pid).or_else(|| super::data::lookup(vid, pid))
}

/// Tier-1 only (the bundled `usb-ids` crate). Use when callers need to
/// distinguish "the offline snapshot knows this" from "we had to fall
/// through to the online overlay" — diagnostics, attribution, etc.
pub fn resolve_bundled(vid: u16, pid: u16) -> Option<UsbInfo> {
    let device = usb_ids::Device::from_vid_pid(vid, pid)?;
    Some(UsbInfo {
        vendor: device.vendor().name().to_string(),
        product: device.name().to_string(),
    })
}

/// `"vendor product (VVVV:PPPP)"` — the canonical display format used by
/// the CLI's `device list`, `device status`, and the daemon's connect /
/// scan log lines. Always returns a non-empty string thanks to [`resolve`]'s
/// synthetic fallback.
pub fn pretty(vid: u16, pid: u16) -> String {
    let info = resolve(vid, pid);
    format!("{} {} ({vid:04X}:{pid:04X})", info.vendor, info.product)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    static OVERLAY_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn bundled_resolves_ftdi_ft232() {
        let info = resolve_bundled(0x0403, 0x6001).expect("FTDI FT232 in bundled DB");
        assert!(
            info.vendor.to_lowercase().contains("future technology"),
            "vendor: {}",
            info.vendor
        );
        assert!(
            info.product.to_lowercase().contains("ft232"),
            "product: {}",
            info.product
        );
    }

    #[test]
    fn bundled_resolves_silabs_cp210x() {
        let info = resolve_bundled(0x10C4, 0xEA60).expect("Silicon Labs CP210x in bundled DB");
        assert!(
            info.vendor.to_lowercase().contains("silicon labs")
                || info.vendor.to_lowercase().contains("cygnal"),
            "vendor: {}",
            info.vendor
        );
        assert!(
            info.product.to_lowercase().contains("cp210"),
            "product: {}",
            info.product
        );
    }

    #[test]
    fn bundled_resolves_wch_ch340() {
        let info = resolve_bundled(0x1A86, 0x7523).expect("WCH CH340 in bundled DB");
        assert!(
            info.vendor.to_lowercase().contains("qinheng")
                || info.vendor.to_lowercase().contains("wch")
                || info.vendor.to_lowercase().contains("nanjing"),
            "vendor: {}",
            info.vendor
        );
        assert!(
            info.product.to_lowercase().contains("ch340")
                || info.product.to_lowercase().contains("serial"),
            "product: {}",
            info.product
        );
    }

    #[test]
    fn unknown_pair_returns_synthetic_placeholder() {
        // 0xFFFE:0xFFFE is reserved and will not be assigned by USB-IF;
        // safe sentinel for "we expect tier-3 to fire."
        let info = resolve(0xFFFE, 0xFFFE);
        assert_eq!(info.vendor, "Unknown vendor 0xFFFE");
        assert_eq!(info.product, "Unknown product 0xFFFE");
    }

    #[test]
    fn pretty_format_uses_canonical_shape() {
        // FTDI FT232 is one of the most stable VID:PIDs in the bundled DB
        // (it's the de-facto USB-serial chip used in every Arduino clone).
        let s = pretty(0x0403, 0x6001);
        assert!(s.ends_with("(0403:6001)"), "tail format wrong: {s}");
        assert!(
            s.to_lowercase().contains("future technology"),
            "missing vendor: {s}"
        );
        // Pretty also handles the unknown path deterministically.
        let unknown = pretty(0xFFFE, 0xFFFE);
        assert_eq!(
            unknown,
            "Unknown vendor 0xFFFE Unknown product 0xFFFE (FFFE:FFFE)"
        );
    }

    #[test]
    fn online_overlay_resolves_when_bundled_misses() {
        let _guard = OVERLAY_LOCK.lock().unwrap();
        // Use a VID:PID that the bundled `usb-ids` crate cannot resolve
        // (0xFFFD:0xABCD is reserved). Install an overlay entry for it and
        // confirm `resolve()` picks tier-2 instead of falling to tier-3.
        assert!(
            resolve_bundled(0xFFFD, 0xABCD).is_none(),
            "test fixture assumed an unallocated VID:PID; pick a different one"
        );
        let mut map = HashMap::new();
        map.insert(
            super::super::data::pack(0xFFFD, 0xABCD),
            UsbInfo {
                vendor: "Acme Test Devices".to_string(),
                product: "Test Widget 9000".to_string(),
            },
        );
        super::super::data::install_online_cache_map(map);

        let info = resolve(0xFFFD, 0xABCD);
        assert_eq!(info.vendor, "Acme Test Devices");
        assert_eq!(info.product, "Test Widget 9000");

        // Reset so unrelated tests don't observe this entry.
        super::super::data::clear_online_cache_for_tests();
    }
}
