//! Tier-2 online overlay: an optional per-VID protobuf map loaded from disk
//! at runtime.
//!
//! Current on-disk schema is `usb-vids.proto.zstd`, published by
//! <https://fastled.github.io/boards/>:
//!
//! ```protobuf
//! message UsbVidDatabase {
//!   repeated Vendor vendors = 1;
//! }
//! message Vendor {
//!   uint32 vid = 1;
//!   string name = 2;
//!   repeated Product products = 3;
//! }
//! message Product {
//!   uint32 pid = 1;
//!   string name = 2;
//! }
//! ```
//!
//! The legacy JSON loader remains for old caches/tests. Its schema was:
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
//! The CLI downloads the zstd-compressed protobuf from FastLED/boards,
//! writes it to the global fbuild cache root, and calls
//! [`install_online_cache_proto_zstd`] to plug it into the resolver.
//! Replacing the cache is supported (`RwLock`, not `OnceLock`) so the
//! daemon/CLI can refresh during a long-running session without a restart.
//!
//! All errors here are swallowed by design — if the overlay can't load, the
//! resolver simply degrades to tier-1 + tier-3.

use super::UsbInfo;
use prost::Message;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

/// Legacy URL of the dataset index produced by the `online-data` branch's
/// nightly workflow.
pub const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/fastled/fbuild/online-data/manifest.json";

/// Legacy JSON overlay URL. Kept for compatibility with older callers and
/// tests; new code should use [`USB_VIDS_PROTO_ZSTD_URL`].
pub const USB_VID_JSON_URL: &str =
    "https://raw.githubusercontent.com/fastled/fbuild/online-data/data/usb-vid.json";

/// Current compact USB VID:PID overlay published by FastLED/boards.
pub const USB_VIDS_PROTO_ZSTD_URL: &str = "https://fastled.github.io/boards/usb-vids.proto.zstd";

static ONLINE_MAP: RwLock<Option<HashMap<u32, UsbInfo>>> = RwLock::new(None);

#[derive(Clone, PartialEq, Message)]
struct UsbVidDatabase {
    #[prost(message, repeated, tag = "1")]
    vendors: Vec<Vendor>,
}

#[derive(Clone, PartialEq, Message)]
struct Vendor {
    #[prost(uint32, tag = "1")]
    vid: u32,
    #[prost(string, tag = "2")]
    name: String,
    #[prost(message, repeated, tag = "3")]
    products: Vec<Product>,
}

#[derive(Clone, PartialEq, Message)]
struct Product {
    #[prost(uint32, tag = "1")]
    pid: u32,
    #[prost(string, tag = "2")]
    name: String,
}

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

/// Install the overlay from the current `usb-vids.proto.zstd` cache file.
/// Silently no-ops on any IO, zstd, or protobuf decode error so USB
/// resolution always degrades to the embedded vendor archive instead of
/// failing port enumeration.
pub fn install_online_cache_proto_zstd(path: &Path) {
    let raw = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::debug!(?path, error = %e, "usb online overlay: read failed");
            return;
        }
    };
    match decode_proto_zstd_bytes(&raw) {
        Ok(map) => {
            let count = map.len();
            install_online_cache_map(map);
            tracing::debug!(
                path = %path.display(),
                entries = count,
                "usb online protobuf overlay installed"
            );
        }
        Err(e) => {
            tracing::warn!(
                ?path,
                error = %e,
                "usb online protobuf overlay decode failed"
            );
        }
    }
}

fn decode_proto_zstd_bytes(raw: &[u8]) -> Result<HashMap<u32, UsbInfo>, String> {
    let mut decoded = Vec::with_capacity(raw.len() * 4);
    zstd::stream::copy_decode(raw, &mut decoded).map_err(|e| format!("zstd: {e}"))?;
    decode_proto_bytes(&decoded)
}

fn decode_proto_bytes(raw: &[u8]) -> Result<HashMap<u32, UsbInfo>, String> {
    let db = UsbVidDatabase::decode(raw).map_err(|e| format!("protobuf: {e}"))?;
    Ok(proto_to_map(db))
}

fn proto_to_map(db: UsbVidDatabase) -> HashMap<u32, UsbInfo> {
    let product_count = db.vendors.iter().map(|vendor| vendor.products.len()).sum();
    let mut packed: HashMap<u32, UsbInfo> = HashMap::with_capacity(product_count);
    for vendor in db.vendors {
        let Ok(vid) = u16::try_from(vendor.vid) else {
            continue;
        };
        for product in vendor.products {
            let Ok(pid) = u16::try_from(product.pid) else {
                continue;
            };
            packed.insert(
                pack(vid, pid),
                UsbInfo {
                    vendor: vendor.name.clone(),
                    product: product.name,
                },
            );
        }
    }
    packed
}

/// Replace the overlay with a pre-built map. Exposed at `pub(crate)` so
/// the daemon could in principle skip the file dance — primary user is the
/// resolver's own test suite.
pub(crate) fn install_online_cache_map(map: HashMap<u32, UsbInfo>) {
    let mut guard = ONLINE_MAP.write().unwrap_or_else(|e| e.into_inner());
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
    fn install_online_cache_proto_zstd_round_trip() {
        let _guard = OVERLAY_LOCK.lock().unwrap();
        clear_online_cache_for_tests();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("usb-vids.proto.zstd");
        let db = UsbVidDatabase {
            vendors: vec![Vendor {
                vid: 0x303A,
                name: "Espressif Systems".to_string(),
                products: vec![
                    Product {
                        pid: 0x0002,
                        name: "ESP32-S2".to_string(),
                    },
                    Product {
                        pid: 0x1001,
                        name: "USB JTAG/serial debug unit".to_string(),
                    },
                ],
            }],
        };
        let mut encoded = Vec::new();
        db.encode(&mut encoded).unwrap();
        let compressed = zstd::stream::encode_all(encoded.as_slice(), 19).unwrap();
        std::fs::write(&path, compressed).unwrap();

        install_online_cache_proto_zstd(&path);

        let a = lookup(0x303A, 0x0002).expect("pid 0002 parsed");
        assert_eq!(a.vendor, "Espressif Systems");
        assert_eq!(a.product, "ESP32-S2");

        let b = lookup(0x303A, 0x1001).expect("pid 1001 parsed");
        assert_eq!(b.vendor, "Espressif Systems");
        assert_eq!(b.product, "USB JTAG/serial debug unit");

        clear_online_cache_for_tests();
    }

    #[test]
    fn install_online_cache_proto_zstd_bad_file_is_silent() {
        let _guard = OVERLAY_LOCK.lock().unwrap();
        clear_online_cache_for_tests();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.proto.zstd");
        std::fs::write(&path, b"not zstd").unwrap();
        install_online_cache_proto_zstd(&path);
        assert!(lookup(0x303A, 0x1001).is_none());
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
