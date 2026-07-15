//! Tier-2 online overlay: an optional per-VID protobuf map loaded from disk
//! at runtime.
//!
//! Current on-disk schema is `usb-vids.proto.zstd`, published by the
//! `fastled/fbuild` `online-data` branch:
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
//! The CLI downloads the zstd-compressed protobuf from `online-data`,
//! writes it to the global fbuild cache root, and calls
//! [`install_online_cache_proto_zstd`] to plug it into the resolver.
//! Replacing the cache is supported (`RwLock`, not `OnceLock`) so the
//! daemon/CLI can refresh during a long-running session without a restart.
//!
//! All errors here are swallowed by design — if the overlay can't load, the
//! resolver simply degrades to tier-1 + tier-3.

use super::UsbInfo;
use prost::Message;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

/// FastLED/boards published registry (canonical USB VID:PID source).
pub const MANIFEST_URL: &str = "https://fastled.github.io/boards/_meta.json";

/// Legacy JSON overlay URL. Kept for compatibility with older callers and
/// tests; new code should use [`USB_VIDS_PROTO_ZSTD_URL`].
pub const USB_VID_JSON_URL: &str = "https://fastled.github.io/boards/usb-ids.json";

/// Current compact USB VID:PID overlay published by the `online-data` branch.
pub const USB_VIDS_PROTO_ZSTD_URL: &str = "https://fastled.github.io/boards/usb-vids.proto.zstd";

static ONLINE_MAP: RwLock<Option<HashMap<u32, UsbInfo>>> = RwLock::new(None);

/// Compile-time-embedded VID:PID → {vendor, product} overlay — the same
/// compact `usb-vids.proto.zstd` the online path fetches, but baked into the
/// binary so full VID:PID resolution works OFFLINE and needs no hardcoded
/// per-board tables. Produced by the FastLED/boards data pipeline
/// (`builders/build_usb_ids.py` over the `platformio`/`arduino`/`vendors`/
/// `other` branches) and vendored here. Refresh workflow: see
/// `crates/fbuild-core/data/README.md`.
// Production never ships a built-in VID/PID catalogue. The canonical
// FastLED/boards artifact is fetched/ingested by the build/runtime cache
// path. Keep the historical blob available only to unit tests as a fixture.
#[cfg(test)]
const EMBEDDED_PROTO: &[u8] = include_bytes!("../../data/usb-vids.proto.zstd");
#[cfg(not(test))]
const EMBEDDED_PROTO: &[u8] = &[];

/// Both projections of the embedded proto. The compact `usb-vids.proto.zstd`
/// carries a `Vendor{vid, name, [Product{pid, name}]}` tree, so a single
/// artifact yields BOTH a VID→vendor map AND a VID:PID→{vendor, product}
/// map — no separate per-VID blob or hardcoded table needed. Parsed exactly
/// once on first use.
#[derive(Default)]
struct EmbeddedOverlay {
    /// VID:PID → {vendor, product}.
    vidpid: HashMap<u32, UsbInfo>,
    /// VID → vendor name (from every `Vendor` entry, even those with no
    /// products listed), so a VID whose exact PID isn't enumerated still
    /// resolves its vendor from the same proto.
    vendors: HashMap<u16, String>,
}

static EMBEDDED: OnceLock<EmbeddedOverlay> = OnceLock::new();

fn embedded() -> &'static EmbeddedOverlay {
    EMBEDDED.get_or_init(|| decode_embedded_overlay(EMBEDDED_PROTO).unwrap_or_default())
}

/// Inflate + parse the embedded proto into both projections. Errors bubble
/// up to `unwrap_or_default()` (empty overlay) so a bad blob degrades to
/// tier-1 vendor resolution rather than crashing.
fn decode_embedded_overlay(raw: &[u8]) -> Result<EmbeddedOverlay, String> {
    let mut decoded = Vec::with_capacity(raw.len() * 4);
    zstd::stream::copy_decode(raw, &mut decoded).map_err(|e| format!("zstd: {e}"))?;
    let db = UsbVidDatabase::decode(decoded.as_slice()).map_err(|e| format!("protobuf: {e}"))?;
    let mut overlay = EmbeddedOverlay::default();
    for vendor in db.vendors {
        let Ok(vid) = u16::try_from(vendor.vid) else {
            continue;
        };
        if !vendor.name.is_empty() {
            overlay.vendors.insert(vid, vendor.name.clone());
        }
        for product in vendor.products {
            let Ok(pid) = u16::try_from(product.pid) else {
                continue;
            };
            overlay.vidpid.insert(
                pack(vid, pid),
                UsbInfo {
                    vendor: vendor.name.clone(),
                    product: product.name,
                },
            );
        }
    }
    Ok(overlay)
}

/// The embedded VID→vendor map (from the same proto). `None` if the VID is
/// absent from the embedded overlay.
pub(crate) fn embedded_vendor(vid: u16) -> Option<&'static str> {
    embedded().vendors.get(&vid).map(|s| s.as_str())
}

/// Number of VID:PID rows in the embedded overlay (test/introspection aid).
pub fn embedded_vidpid_count() -> usize {
    embedded().vidpid.len()
}

/// Number of VID→vendor rows in the embedded overlay (test/introspection aid).
pub fn embedded_vendor_count() -> usize {
    embedded().vendors.len()
}

/// 7-day cache TTL. The `online-data` branch refreshes nightly; a weekly
/// local refresh gives useful freshness without adding network cost to every
/// serial-port operation.
pub const ONLINE_CACHE_TTL_SECS: u64 = 7 * 24 * 60 * 60;

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

/// Install the overlay from a JSON file on disk. Replaces any previously
/// installed overlay. Silently no-ops on any IO or parse error so the
/// resolver never crashes on a stale / partial cache file.
pub fn install_online_cache(path: &Path) {
    let _ = try_install_online_cache(path);
}

/// Same as [`install_online_cache`], but reports whether an overlay was
/// successfully parsed and installed.
pub fn try_install_online_cache(path: &Path) -> bool {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(?path, error = %e, "usb online overlay: read failed");
            return false;
        }
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(?path, error = %e, "usb online overlay: parse failed");
            return false;
        }
    };
    // Flatten the on-disk per-VID nested shape into the O(1) flat
    // `(vid, pid) -> UsbInfo` lookup table the resolver expects.
    let Some(parsed) = parsed.as_object() else {
        tracing::warn!(?path, "usb online overlay: root is not an object");
        return false;
    };
    let mut packed: HashMap<u32, UsbInfo> = HashMap::with_capacity(parsed.len() * 4);
    for (vid_str, value) in parsed {
        let Some(vid) = parse_hex_u16(vid_str) else {
            continue;
        };
        // Accept fbuild's legacy {vendor, products:[[pid,name]]} shape and
        // the canonical FastLED/boards {"Vendor name":..., "PIDs":[{pid:name}]}
        // shape. Keeping this compatibility at the boundary lets boards remain
        // the single source of truth without a second generated fbuild file.
        let vendor = value.get("vendor").and_then(Value::as_str)
            .or_else(|| value.get("Vendor name").and_then(Value::as_str))
            .unwrap_or("Unknown USB vendor");
        if let Some(products) = value.get("products").and_then(Value::as_array) {
            for pair in products {
                let Some(items) = pair.as_array() else { continue };
                if items.len() != 2 { continue; }
                let Some(pid_str) = items[0].as_str() else { continue };
                let Some(product_name) = items[1].as_str() else { continue };
                let Some(pid) = parse_hex_u16(pid_str) else { continue; };
                packed.insert(pack(vid, pid), UsbInfo { vendor: vendor.to_string(), product: product_name.to_string() });
            }
        }
        if let Some(products) = value.get("PIDs").and_then(Value::as_array) {
            for item in products {
                let Some(map) = item.as_object() else { continue };
                for (pid_str, product_name) in map {
                    let Some(product_name) = product_name.as_str() else { continue };
                    let Some(pid) = parse_hex_u16(pid_str) else { continue; };
                    packed.insert(pack(vid, pid), UsbInfo { vendor: vendor.to_string(), product: product_name.to_string() });
                }
            }
        }
    }
    let count = packed.len();
    install_online_cache_map(packed);
    tracing::debug!(path = %path.display(), entries = count, "usb online overlay installed");
    true
}

/// Install the overlay from the current `usb-vids.proto.zstd` cache file.
/// Silently no-ops on any IO, zstd, or protobuf decode error so USB
/// resolution always degrades to the embedded vendor archive instead of
/// failing port enumeration.
pub fn install_online_cache_proto_zstd(path: &Path) {
    let _ = try_install_online_cache_proto_zstd(path);
}

/// Same as [`install_online_cache_proto_zstd`], but reports whether an
/// overlay was successfully decoded and installed.
pub fn try_install_online_cache_proto_zstd(path: &Path) -> bool {
    let raw = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::debug!(?path, error = %e, "usb online overlay: read failed");
            return false;
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
            true
        }
        Err(e) => {
            tracing::warn!(
                ?path,
                error = %e,
                "usb online protobuf overlay decode failed"
            );
            false
        }
    }
}

/// Populate and install the runtime USB overlay from cache paths.
///
/// The compact protobuf/zstd artifact is preferred. If that fetch or decode
/// fails, the legacy JSON dataset is fetched/installed as a compatibility
/// fallback. This is intentionally path-driven so callers can use
/// `fbuild-paths` without creating a dependency cycle in `fbuild-core`.
pub fn populate_online_cache_from_paths(proto_cache_path: &Path, json_cache_path: &Path) -> bool {
    populate_online_cache_from_paths_and_urls(
        proto_cache_path,
        json_cache_path,
        USB_VIDS_PROTO_ZSTD_URL,
        USB_VID_JSON_URL,
    )
}

/// Same as [`populate_online_cache_from_paths`], with injectable URLs for
/// tests and local mirrors.
pub fn populate_online_cache_from_paths_and_urls(
    proto_cache_path: &Path,
    json_cache_path: &Path,
    proto_url: &str,
    json_url: &str,
) -> bool {
    if !cache_is_fresh(proto_cache_path) {
        if let Err(e) = fetch_overlay_to(proto_cache_path, proto_url) {
            tracing::debug!(
                error = %e,
                "usb online protobuf overlay fetch failed; trying JSON overlay"
            );
        }
    }
    if try_install_online_cache_proto_zstd(proto_cache_path) {
        return true;
    }

    if !cache_is_fresh(json_cache_path) {
        if let Err(e) = fetch_overlay_to(json_cache_path, json_url) {
            tracing::debug!(error = %e, "usb online JSON overlay fetch failed");
        }
    }
    try_install_online_cache(json_cache_path)
}

fn cache_is_fresh(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(age) = modified.elapsed() else {
        return false;
    };
    age.as_secs() < ONLINE_CACHE_TTL_SECS
}

fn fetch_overlay_to(path: &Path, url: &str) -> Result<(), String> {
    let path = path.to_path_buf();
    let url = url.to_string();
    std::thread::spawn(move || fetch_overlay_to_inner(&path, &url))
        .join()
        .map_err(|_| "fetch thread panicked".to_string())?
}

fn fetch_overlay_to_inner(path: &Path, url: &str) -> Result<(), String> {
    let client = crate::http::blocking_client(Duration::from_secs(15));
    fetch_overlay_to_inner_with_client(path, &client, url)
}

fn fetch_overlay_to_inner_with_client(
    path: &Path,
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<(), String> {
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("http get: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("http status {}", response.status()));
    }
    let body = response.bytes().map_err(|e| format!("body read: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("tmp write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    tracing::debug!(
        path = %path.display(),
        size = body.len(),
        "usb online overlay cache refreshed"
    );
    Ok(())
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

/// Runtime online overlay only (freshest, curated at workflow time).
pub(crate) fn online_lookup(vid: u16, pid: u16) -> Option<UsbInfo> {
    let key = pack(vid, pid);
    let guard = ONLINE_MAP.read().ok()?;
    guard.as_ref()?.get(&key).cloned()
}

/// Compile-time embedded overlay only (FastLED/boards curated device map).
pub(crate) fn embedded_lookup(vid: u16, pid: u16) -> Option<UsbInfo> {
    embedded().vidpid.get(&pack(vid, pid)).cloned()
}

/// Combined tier-2 lookup (online overlay, then embedded overlay). Retained
/// for the test suite; production resolution goes through the layered
/// [`super::resolver::try_resolve`], which consults `online_lookup` and
/// `embedded_lookup` separately so it can source the vendor authoritatively.
#[cfg(test)]
pub(crate) fn lookup(vid: u16, pid: u16) -> Option<UsbInfo> {
    online_lookup(vid, pid).or_else(|| embedded_lookup(vid, pid))
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
    let mut guard = ONLINE_MAP.write().unwrap_or_else(|e| e.into_inner());
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
        // A bad file installs no ONLINE overlay. Probe a synthetic pair that
        // is absent from the compile-time-embedded overlay too, so `None`
        // proves the bad file was silently ignored (a real embedded pair
        // would resolve from the base layer and mask the check).
        assert!(lookup(0xEEEE, 0x1234).is_none());
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
    fn populate_online_cache_falls_back_to_json_when_proto_is_missing() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let _guard = OVERLAY_LOCK.lock().unwrap();
        clear_online_cache_for_tests();

        let tmp = tempfile::tempdir().unwrap();
        let proto_path = tmp.path().join("usb-vids.proto.zstd");
        let json_path = tmp.path().join("usb-vid.json");

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
                        let mut buf = [0_u8; 1024];
                        let _ = stream.read(&mut buf).unwrap();
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

        let installed = populate_online_cache_from_paths_and_urls(
            &proto_path,
            &json_path,
            &format!("http://{addr}/usb-vids.proto.zstd"),
            &format!("http://{addr}/usb-vid.json"),
        );

        assert_eq!(handle.join().unwrap(), 2);
        assert!(installed, "JSON fallback should install the overlay");
        assert_eq!(std::fs::read_to_string(&json_path).unwrap(), json);

        let info = lookup(0xFEED, 0xC0DE).expect("json fallback overlay lookup");
        assert_eq!(info.vendor, "Feedface Inc");
        assert_eq!(info.product, "Coded Widget");

        clear_online_cache_for_tests();
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

    #[test]
    fn install_online_cache_accepts_fastled_boards_shape() {
        let _guard = OVERLAY_LOCK.lock().unwrap();
        clear_online_cache_for_tests();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("boards-usb-ids.json");
        std::fs::write(
            &path,
            r#"{"2e8a":{"Vendor name":"Raspberry Pi","PIDs":[{"0003":"Raspberry Pi RP2 BOOTSEL"},{"000a":"Raspberry Pi Pico"},{"000f":"Raspberry Pi Pico 2"}]}}"#,
        )
        .unwrap();
        assert!(try_install_online_cache(&path));
        assert_eq!(lookup(0x2e8a, 0x0003).unwrap().product, "Raspberry Pi RP2 BOOTSEL");
        assert_eq!(lookup(0x2e8a, 0x000f).unwrap().product, "Raspberry Pi Pico 2");
        clear_online_cache_for_tests();
    }
}
