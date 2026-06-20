//! Tier-2 online overlay: an optional `{ "VVVV:PPPP": {vendor, product} }`
//! JSON map loaded from disk at runtime.
//!
//! The daemon (or a CLI command) downloads the JSON from the repo's
//! `online-data` branch, writes it to a cache path, and calls
//! [`install_online_cache`] to plug it into the resolver. Replacing the
//! cache is supported (`RwLock`, not `OnceLock`) so the daemon can refresh
//! during a long-running session without a restart.
//!
//! All errors here are swallowed by design — if the overlay can't load, the
//! resolver simply degrades to tier-1 + tier-3.

use super::UsbInfo;
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
    let parsed: HashMap<String, UsbInfo> = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(?path, error = %e, "usb online overlay: parse failed");
            return;
        }
    };
    let mut packed = HashMap::with_capacity(parsed.len());
    for (key, info) in parsed {
        if let Some(packed_key) = parse_vid_pid_key(&key) {
            packed.insert(packed_key, info);
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

fn parse_vid_pid_key(key: &str) -> Option<u32> {
    let (vid_s, pid_s) = key.split_once(':')?;
    let vid = u16::from_str_radix(vid_s.trim(), 16).ok()?;
    let pid = u16::from_str_radix(pid_s.trim(), 16).ok()?;
    Some(pack(vid, pid))
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
        let json = r#"{
            "feed:c0de": {"vendor": "Feedface Inc", "product": "Coded Widget"},
            "FEED:F00D": {"vendor": "Feedface Inc", "product": "Food Sensor"}
        }"#;
        std::fs::write(&path, json).unwrap();

        install_online_cache(&path);

        // Lowercase key
        let a = lookup(0xFEED, 0xC0DE).expect("lowercase key parsed");
        assert_eq!(a.vendor, "Feedface Inc");
        assert_eq!(a.product, "Coded Widget");

        // Uppercase key
        let b = lookup(0xFEED, 0xF00D).expect("uppercase key parsed");
        assert_eq!(b.product, "Food Sensor");

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
}
