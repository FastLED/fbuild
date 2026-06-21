//! Tiered USB VID:PID → name resolver. See the [crate::usb] module-level
//! documentation for the design.

use serde::{Deserialize, Serialize};

use super::embedded;

/// Resolved USB device identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbInfo {
    pub vendor: String,
    pub product: String,
}

/// Best-effort lookup. Never returns `None`: a synthetic
/// `"Unknown vendor 0xVVVV"` / `"Unknown product 0xPPPP"` is produced
/// when both tier-1 (embedded vendor archive) and tier-2 (online overlay)
/// miss.
pub fn resolve(vid: u16, pid: u16) -> UsbInfo {
    try_resolve(vid, pid).unwrap_or_else(|| UsbInfo {
        vendor: format!("Unknown vendor 0x{vid:04X}"),
        product: format!("Unknown product 0x{pid:04X}"),
    })
}

/// Tier-1 + tier-2 only. Returns `None` if neither knows this pair.
///
/// Tier order is reversed from the old `usb-ids`-backed implementation:
/// the online overlay carries the full `{vendor, product}` aggregate
/// (it ingests the bundled Rust crate dump on the `online-data` branch
/// at workflow time), while the embedded vendor archive is intentionally
/// vendor-name-only. We consult the overlay first because it has more
/// information; we only fall through to the embedded archive when the
/// overlay misses the VID entirely.
pub fn try_resolve(vid: u16, pid: u16) -> Option<UsbInfo> {
    if let Some(info) = super::data::lookup(vid, pid) {
        return Some(info);
    }
    resolve_bundled(vid, pid)
}

/// Tier-1 only (the compile-time-embedded vendor archive). The embedded
/// archive carries vendor names only — see
/// `crates/fbuild-core/data/usb-vendors.tar.zst`. For VIDs present in the
/// archive, the returned `UsbInfo.product` is a synthetic `"Device 0xPPPP"`
/// placeholder since per-PID resolution lives in the runtime overlay
/// (tier-2) and the www-branch SQLite-over-HTTP database.
pub fn resolve_bundled(vid: u16, pid: u16) -> Option<UsbInfo> {
    embedded::vendor_name(vid).map(|vendor| UsbInfo {
        vendor: vendor.to_string(),
        product: format!("Device 0x{pid:04X}"),
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
    fn embedded_resolves_ftdi_vendor() {
        let info = resolve_bundled(0x0403, 0x6001).expect("FTDI VID in embedded archive");
        assert!(
            info.vendor.to_lowercase().contains("future technology")
                || info.vendor.to_lowercase().contains("ftdi"),
            "vendor: {}",
            info.vendor
        );
        // Tier-1 product is intentionally synthetic — the real product
        // name lives in the runtime overlay (tier-2).
        assert_eq!(info.product, "Device 0x6001");
    }

    #[test]
    fn embedded_resolves_silabs_vendor() {
        let info = resolve_bundled(0x10C4, 0xEA60).expect("Silicon Labs VID in embedded archive");
        assert!(
            info.vendor.to_lowercase().contains("silicon lab")
                || info.vendor.to_lowercase().contains("cygnal"),
            "vendor: {}",
            info.vendor
        );
    }

    #[test]
    fn embedded_resolves_espressif_via_inlined_supplement() {
        // 0x303a is missing from every canonical text database we mirror
        // — the curated inlined supplement (online-data-tools/
        // vendor_names_inlined.py) injects it during the workflow's
        // merge step, and the resulting tar.zst is the embedded archive.
        // This test pins the round-trip: curated overlay → embedded
        // archive → fbuild runtime resolution.
        let info = resolve_bundled(0x303A, 0x4002).expect("Espressif in embedded archive");
        assert!(
            info.vendor.to_lowercase().contains("espressif"),
            "vendor: {}",
            info.vendor
        );
    }

    #[test]
    fn unknown_pair_returns_synthetic_placeholder() {
        // 0xBADD:0xBADD is reserved and will not be assigned by USB-IF.
        let info = resolve(0xBADD, 0xBADD);
        assert_eq!(info.vendor, "Unknown vendor 0xBADD");
        assert_eq!(info.product, "Unknown product 0xBADD");
    }

    #[test]
    fn pretty_format_uses_canonical_shape() {
        // FTDI is in the embedded archive — vendor resolves, product is
        // synthetic so the tail is deterministic.
        let s = pretty(0x0403, 0x6001);
        assert!(s.ends_with("(0403:6001)"), "tail format wrong: {s}");
        assert!(
            s.to_lowercase().contains("future technology")
                || s.to_lowercase().contains("ftdi"),
            "missing vendor: {s}"
        );
        // Unknown path stays deterministic.
        let unknown = pretty(0xBADD, 0xBADD);
        assert_eq!(
            unknown,
            "Unknown vendor 0xBADD Unknown product 0xBADD (BADD:BADD)"
        );
    }

    #[test]
    fn online_overlay_resolves_when_embedded_misses() {
        let _guard = OVERLAY_LOCK.lock().unwrap();
        // Pick a VID:PID that the embedded archive cannot resolve.
        // 0xFFFD is reserved by USB-IF.
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

        super::super::data::clear_online_cache_for_tests();
    }

    #[test]
    fn online_overlay_wins_over_embedded_for_same_vid() {
        // Overlay has tier priority — if a VID is in BOTH the embedded
        // archive and the overlay, the overlay's richer entry wins.
        let _guard = OVERLAY_LOCK.lock().unwrap();
        let mut map = HashMap::new();
        map.insert(
            super::super::data::pack(0x0403, 0x6001),
            UsbInfo {
                vendor: "FTDI Official".to_string(),
                product: "FT232 Serial Converter".to_string(),
            },
        );
        super::super::data::install_online_cache_map(map);
        let info = resolve(0x0403, 0x6001);
        assert_eq!(info.vendor, "FTDI Official");
        assert_eq!(info.product, "FT232 Serial Converter");
        super::super::data::clear_online_cache_for_tests();
    }
}
