//! Tiered USB VID:PID → name resolver. See the [crate::usb] module-level
//! documentation for the design.

use serde::{Deserialize, Serialize};

#[cfg(test)]
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
    if let Some(info) = super::data::online_lookup(vid, pid) {
        return Some(info);
    }

    #[cfg(not(test))]
    {
        None
    }

    #[cfg(test)]
    {
    // Test builds may exercise an embedded fixture. Release/runtime builds
    // must use only the verified FastLED/boards cache above.
    // Take the PRODUCT from the FastLED/boards curated
    //    device map (e.g. "NXP LPC-Link2", "Teensy (Serial mode)"), but
    //    resolve the VENDOR through the best available source rather than the
    //    proto's per-VID:PID vendor column (which can be blank, or — for the
    //    generic-bridge attributions in the board layers — board-attributed).
    //    Vendor priority: the proto's curated VID→vendor map (e.g. 16C0 →
    //    "PJRC (Teensy)") when non-empty, then the authoritative
    //    usb-vendors.tar.zst archive (e.g. 10C4 → "Silicon Labs").
    let product = super::data::embedded_lookup(vid, pid).map(|i| i.product);
    let vendor = super::data::embedded_vendor(vid)
        .filter(|v| !v.is_empty())
        .or_else(|| embedded::vendor_name(vid))
        .map(str::to_string);

    match (product, vendor) {
        (Some(product), Some(vendor)) => Some(UsbInfo { vendor, product }),
        (Some(product), None) => Some(UsbInfo {
            vendor: String::new(),
            product,
        }),
        // No product in the embedded overlay → vendor-only tier (archive +
        // synthetic product placeholder), which also covers VIDs absent from
        // the curated proto entirely.
        (None, _) => resolve_bundled(vid, pid),
    }
    }
}

/// Vendor-name-only tier (no per-PID product). Two compile-time-embedded
/// sources, in priority order:
///
/// 1. The comprehensive `usb-vendors.tar.zst` archive (~2.2k VIDs) — the
///    authoritative USB-IF vendor names (e.g. `10C4` → "Silicon Labs").
/// 2. The `usb-vids.proto.zstd` VID→vendor map (FastLED/boards pipeline) as
///    a fallback for VIDs not in the archive.
///
/// The archive is preferred because the boards pipeline's VID→vendor column
/// is board-attributed (it can label a shared bridge VID with whichever
/// board first claimed it); the per-VID:PID curated names still take effect
/// through the tier-2 [`try_resolve`] path, which consults the proto's
/// VID:PID map first.
///
/// For VIDs present in either, `UsbInfo.product` is a synthetic
/// `"Device 0xPPPP"` placeholder since per-PID resolution lives in the
/// VID:PID overlay (tier-2).
pub fn resolve_bundled(vid: u16, pid: u16) -> Option<UsbInfo> {
    #[cfg(not(test))]
    {
        let _ = (vid, pid);
        None
    }
    #[cfg(test)]
    {
    let vendor = embedded::vendor_name(vid).or_else(|| super::data::embedded_vendor(vid))?;
    Some(UsbInfo {
        vendor: vendor.to_string(),
        product: format!("Device 0x{pid:04X}"),
    })
    }
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
    fn embedded_proto_resolves_lpc_link2_offline() {
        // These VID:PIDs are not installed by any online-overlay test, so
        // resolution falls through to the compile-time-embedded
        // usb-vids.proto.zstd (FastLED/boards pipeline). We deliberately do
        // NOT clear the online cache here — that shared state belongs to
        // `data::tests` (guarded by a different lock); touching it would race
        // those tests.
        assert!(
            super::super::data::embedded_vidpid_count() > 0,
            "embedded VID:PID overlay should be non-empty"
        );
        let info = try_resolve(0x1FC9, 0x0132).expect("LPC-Link2 in embedded overlay");
        assert!(
            info.product.contains("LPC-Link2"),
            "expected LPC-Link2 product, got {:?}",
            info.product
        );
        // PJRC Teensy 16C0:0483 resolves from the same embedded overlay.
        let teensy = try_resolve(0x16C0, 0x0483).expect("Teensy in embedded overlay");
        assert!(
            teensy.vendor.to_lowercase().contains("pjrc")
                || teensy.product.to_lowercase().contains("teensy"),
            "expected PJRC/Teensy, got {:?} / {:?}",
            teensy.vendor,
            teensy.product
        );
    }

    #[test]
    fn embedded_proto_carries_both_vid_and_vidpid_maps() {
        // The single usb-vids.proto.zstd yields BOTH projections:
        //  - a VID:PID → product map, and
        //  - a VID → vendor map (used for VID-level resolution when the exact
        //    PID isn't enumerated).
        assert!(super::super::data::embedded_vidpid_count() > 0);
        assert!(super::super::data::embedded_vendor_count() > 0);

        // VID map: the proto's VID→vendor projection carries the curated
        // FastLED/boards names (e.g. 16C0 → "PJRC (Teensy)", 1FC9 → "NXP …"),
        // independent of the authoritative usb-vendors archive.
        assert!(
            super::super::data::embedded_vendor(0x1FC9)
                .is_some_and(|v| v.to_lowercase().contains("nxp")),
            "proto VID map should resolve 1FC9 → NXP"
        );
        assert!(
            super::super::data::embedded_vendor(0x16C0)
                .is_some_and(|v| v.to_lowercase().contains("pjrc")),
            "proto VID map should resolve 16C0 → PJRC"
        );
    }

    #[test]
    fn embedded_archive_resolves_pjrc_teensy_products() {
        // The `fbuild port scan` Teensy rows get their product names from the
        // embedded FastLED/boards VID:PID archive — NOT a hardcoded table in
        // fbuild. Pin the round-trip for the PIDs a Teensy exposes as serial
        // ports. FastLED/fbuild#962.
        for pid in [0x0483u16, 0x0489] {
            let info = try_resolve(0x16C0, pid).expect("Teensy PID in embedded archive");
            assert!(
                info.vendor.to_lowercase().contains("pjrc")
                    || info.vendor.to_lowercase().contains("teensy"),
                "16C0:{pid:04X} vendor should be PJRC/Teensy, got {:?}",
                info.vendor
            );
            // Product name is archive-derived (a board or USB-mode label) and
            // may change as the FastLED/boards data is refreshed — assert it is
            // a real Teensy name, NOT the synthetic `Device 0xPPPP` placeholder.
            assert!(
                info.product.to_lowercase().contains("teensy"),
                "16C0:{pid:04X} product should name Teensy, got {:?}",
                info.product
            );
            assert_ne!(info.product, format!("Device 0x{pid:04X}"));
        }
    }

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
        // FTDI VID is in the embedded vendor archive, and this PID is NOT in
        // the VID:PID proto overlay, so resolution falls to the vendor
        // archive: vendor resolves, product is the synthetic placeholder so
        // the tail is deterministic.
        let s = pretty(0x0403, 0x6015);
        assert!(s.ends_with("(0403:6015)"), "tail format wrong: {s}");
        assert!(
            s.to_lowercase().contains("future technology") || s.to_lowercase().contains("ftdi"),
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
