//! Tier-2 online overlay: an optional per-VID JSON map loaded from disk
//! at runtime.
//!
//! Schema on disk (from `online-data/data/usb-vid.json`):
//!
//! ```json
//! {
//!   "0403": {
//!     "vendor": "Future Technology Devices International, Ltd",
//!     "products": [
//!       ["6001", "FT232 Serial (UART) IC"],
//!       ["6010", "FT2232C/D/H Dual UART/FIFO IC"]
//!     ]
//!   },
//!   "10c4": {
//!     "vendor": "Silicon Labs",
//!     "products": [["ea60", "CP210x UART Bridge"]]
//!   }
//! }
//! ```
//!
//! `products` is a list of two-element `[pid, product_name]` arrays
//! sorted by pid for stable diffs.
//!
//! Internally we still flatten that into a single `HashMap<u32, UsbInfo>`
//! keyed by `(vid << 16) | pid` for O(1) `(vid, pid)` lookup; the nested
//! shape on disk just avoids duplicating the vendor name for every
//! product entry under a VID (significantly smaller payload).
//!
//! The daemon downloads the JSON from the repo's `online-data` branch,
//! writes it to a cache path, and calls [`install_online_cache`] to plug
//! it into the resolver. Replacing the cache is supported (`RwLock`, not
//! `OnceLock`) so the daemon can refresh during a long-running session
//! without a restart.
//!
//! All errors here are swallowed by design — if the overlay can't load, the
//! resolver simply degrades to tier-1 + tier-3.

use super::UsbInfo;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

/// URL of the dataset index produced by the `online-data` branch's nightly
/// workflow. Clients can `GET` this, parse the JSON, and pull the
/// `datasets["usb-vid"].url` field to find the live `usb-vid.json`.
pub const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/fastled/fbuild/online-data/manifest.json";

/// Direct convenience URL for the merged dataset itself. Kept in sync with
/// [`MANIFEST_URL`]'s `datasets["usb-vid"].url` by the nightly workflow.
/// Clients that don't want to parse the manifest can fetch this directly.
pub const USB_VID_JSON_URL: &str =
    "https://raw.githubusercontent.com/fastled/fbuild/online-data/data/usb-vid.json";

static ONLINE_MAP: RwLock<Option<HashMap<u32, UsbInfo>>> = RwLock::new(None);

/// On-disk representation: one entry per VID, with the vendor name shared
/// across all products of that VID. Each product is a two-element
/// `[pid_hex, product_name]` tuple.
#[derive(Debug, Deserialize)]
struct VendorEntry {
    vendor: String,
    #[serde(default)]
    products: Vec<(String, String)>,
}

/// Install the overlay from a JSON file on disk. Replaces any previously
/// installed overlay. Silently no-ops on any IO or parse error so the
/// resolver never crashes on a stale / partial cache file.
pub fn install_online_cache(path: &Path) {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(?path, error = %e, "usb online overlay: read failed");
            return;
        }
    };
    let parsed: HashMap<String, VendorEntry> = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(?path, error = %e, "usb online overlay: parse failed");
            return;
        }
    };
    // Flatten the on-disk per-VID nested shape into the O(1) flat
    // `(vid, pid) -> UsbInfo` lookup table the resolver expects.
    let mut packed: HashMap<u32, UsbInfo> = HashMap::with_capacity(parsed.len() * 4);
    for (vid_str, entry) in parsed {
        let Some(vid) = parse_hex_u16(&vid_str) else {
            continue;
        };
        for (pid_str, product_name) in entry.products {
            let Some(pid) = parse_hex_u16(&pid_str) else {
                continue;
            };
            packed.insert(
                pack(vid, pid),
                UsbInfo {
                    vendor: entry.vendor.clone(),
                    product: product_name,
                },
            );
        }
    }
    let count = packed.len();
    install_online_cache_map(packed);
    tracing::debug!(path = %path.display(), entries = count, "usb online overlay installed");
}

/// Replace the overlay with a pre-built map. Exposed at `pub(crate)` so
/// the daemon could in principle skip the file dance — primary user is the
/// resolver's own test suite.
pub(crate) fn install_online_cache_map(map: HashMap<u32, UsbInfo>) {
    let mut guard = ONLINE_MAP.write().unwrap();
    *guard = Some(map);
}

/// Tier-2 lookup. `None` if no overlay is installed or the pair is missing.
pub(crate) fn lookup(vid: u16, pid: u16) -> Option<UsbInfo> {
    let guard = ONLINE_MAP.read().ok()?;
    let map = guard.as_ref()?;
    map.get(&pack(vid, pid)).cloned()
}

/// Pack a (vid, pid) into a single `u32` key. The high half is the vendor.
pub(crate) fn pack(vid: u16, pid: u16) -> u32 {
    ((vid as u32) << 16) | (pid as u32)
}

fn parse_hex_u16(s: &str) -> Option<u16> {
    u16::from_str_radix(s.trim(), 16).ok()
}

#[cfg(test)]
pub(crate) fn clear_online_cache_for_tests() {
    let mut guard = ONLINE_MAP.write().unwrap();
    *guard = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static OVERLAY_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn install_online_cache_from_file_round_trip() {
        let _guard = OVERLAY_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("usb-vid.json");
        // Nested per-VID shape (the format published on the
        // `online-data` branch starting with the multi-dataset rev).
        let json = r#"{
            "feed": {
                "vendor": "Feedface Inc",
                "products": [
                    ["c0de", "Coded Widget"],
                    ["F00D", "Food Sensor"]
                ]
            },
            "DEAD": {
                "vendor": "Acme",
                "products": [["BEEF", "Beef Widget"]]
            }
        }"#;
        std::fs::write(&path, json).unwrap();

        install_online_cache(&path);

        // Lowercase pid
        let a = lookup(0xFEED, 0xC0DE).expect("lowercase pid parsed");
        assert_eq!(a.vendor, "Feedface Inc");
        assert_eq!(a.product, "Coded Widget");

        // Uppercase pid under the same vendor (vendor name shared)
        let b = lookup(0xFEED, 0xF00D).expect("uppercase pid parsed");
        assert_eq!(b.vendor, "Feedface Inc");
        assert_eq!(b.product, "Food Sensor");

        // Uppercase vid + uppercase pid
        let c = lookup(0xDEAD, 0xBEEF).expect("uppercase vid parsed");
        assert_eq!(c.vendor, "Acme");
        assert_eq!(c.product, "Beef Widget");

        clear_online_cache_for_tests();
    }

    #[test]
    fn install_online_cache_missing_file_is_silent() {
        let _guard = OVERLAY_LOCK.lock().unwrap();
        clear_online_cache_for_tests();
        let path = std::path::PathBuf::from("/nonexistent/path/usb-vid.json");
        // Must not panic.
        install_online_cache(&path);
        // No overlay installed → lookup returns None.
        assert!(lookup(0x1234, 0x5678).is_none());
    }

    #[test]
    fn install_online_cache_bad_json_is_silent() {
        let _guard = OVERLAY_LOCK.lock().unwrap();
        clear_online_cache_for_tests();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.json");
        std::fs::write(&path, "this is not json {").unwrap();
        install_online_cache(&path); // must not panic
        assert!(lookup(0x1234, 0x5678).is_none());
    }

    #[test]
    fn install_online_cache_vendor_without_products_is_skipped() {
        let _guard = OVERLAY_LOCK.lock().unwrap();
        clear_online_cache_for_tests();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.json");
        // Vendor known but no products listed — entry shouldn't crash
        // the loader; it just contributes zero `(vid, pid)` rows.
        std::fs::write(&path, r#"{"feed": {"vendor": "Foo", "products": []}}"#).unwrap();
        install_online_cache(&path);
        assert!(lookup(0xFEED, 0xC0DE).is_none());
        clear_online_cache_for_tests();
    }
}
