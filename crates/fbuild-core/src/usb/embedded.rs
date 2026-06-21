//! Compile-time-embedded USB VID → vendor-name map.
//!
//! Replaces the runtime dependency on the `usb-ids` Rust crate. The blob is
//! produced by `online-data-tools/build_vendor_archive.py` and lives at
//! `crates/fbuild-core/data/usb-vendors.tar.zst`. See that script + the
//! `data/README.md` for the refresh workflow.
//!
//! Compact format inside the tar (`usb-vendors.txt`):
//! ```text
//! vid:vendor,vid:vendor,...
//! ```
//! where `vid` is 4-hex-digit lowercase and `vendor` has `,` and `%`
//! escaped per RFC 3986. See `parse_compact` for the inflater and
//! `online-data-tools/build_vendor_archive.py::pack_compact` for the
//! producer counterpart.
//!
//! Lookup is `O(1)` after the first call: the tar is decompressed +
//! parsed exactly once into a `HashMap<u16, String>` behind a `OnceLock`.
//! Decompression cost is paid lazily — callers that never touch USB
//! resolution don't pay it at all.

use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

/// Lock-step with `build_vendor_archive.py::SCHEMA_VERSION`. Bump both
/// sides whenever the archive layout changes; the consumer refuses to
/// load an archive whose schema is newer than this constant.
pub const EMBEDDED_SCHEMA_VERSION: u64 = 2;

const RAW_ARCHIVE: &[u8] = include_bytes!("../../data/usb-vendors.tar.zst");

static VENDOR_MAP: OnceLock<HashMap<u16, String>> = OnceLock::new();

/// Look up the vendor name for a USB VID. Returns `None` if the embedded
/// archive doesn't carry that VID — callers should fall through to the
/// online overlay (`usb::data::lookup`) before reporting "unknown".
pub fn vendor_name(vid: u16) -> Option<&'static str> {
    VENDOR_MAP
        .get_or_init(load_or_panic)
        .get(&vid)
        .map(|s| s.as_str())
}

/// Number of vendor entries in the embedded archive. Mostly useful in
/// tests to detect accidental truncation.
pub fn embedded_vendor_count() -> usize {
    VENDOR_MAP.get_or_init(load_or_panic).len()
}

fn load_or_panic() -> HashMap<u16, String> {
    match load() {
        Ok(m) => m,
        Err(e) => {
            // A corrupt embedded archive is a build-config bug, not a
            // runtime condition we can recover from. Panicking here surfaces
            // it loudly the first time anything in fbuild touches a USB
            // device rather than silently degrading to "unknown vendor".
            panic!("fbuild-core: embedded usb-vendors.tar.zst is unusable: {e}");
        }
    }
}

#[derive(Debug)]
enum LoadError {
    Zstd(String),
    Tar(String),
    MissingPayload,
    SchemaTooNew { found: u64, max: u64 },
    BadManifest(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zstd(s) => write!(f, "zstd decompress failed: {s}"),
            Self::Tar(s) => write!(f, "tar extract failed: {s}"),
            Self::MissingPayload => f.write_str("archive missing usb-vendors.txt"),
            Self::SchemaTooNew { found, max } => write!(
                f,
                "embedded archive schema_version={found} exceeds consumer max={max}; \
                 bump EMBEDDED_SCHEMA_VERSION in fbuild-core::usb::embedded after \
                 confirming the consumer supports the new format"
            ),
            Self::BadManifest(s) => write!(f, "manifest.json invalid: {s}"),
        }
    }
}

fn load() -> Result<HashMap<u16, String>, LoadError> {
    let mut decoded = Vec::with_capacity(RAW_ARCHIVE.len() * 8);
    zstd::stream::copy_decode(RAW_ARCHIVE, &mut decoded)
        .map_err(|e| LoadError::Zstd(e.to_string()))?;

    let mut payload: Option<String> = None;
    let mut manifest: Option<String> = None;
    let mut archive = tar::Archive::new(decoded.as_slice());
    for entry in archive.entries().map_err(|e| LoadError::Tar(e.to_string()))? {
        let mut entry = entry.map_err(|e| LoadError::Tar(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| LoadError::Tar(e.to_string()))?
            .to_string_lossy()
            .into_owned();
        let mut buf = String::new();
        entry
            .read_to_string(&mut buf)
            .map_err(|e| LoadError::Tar(e.to_string()))?;
        match path.as_str() {
            "usb-vendors.txt" => payload = Some(buf),
            "manifest.json" => manifest = Some(buf),
            _ => {} // forward-compat — ignore unknown extras
        }
    }

    if let Some(m) = manifest {
        let parsed: serde_json::Value =
            serde_json::from_str(&m).map_err(|e| LoadError::BadManifest(e.to_string()))?;
        let v = parsed
            .get("schema_version")
            .and_then(|x| x.as_u64())
            .ok_or_else(|| LoadError::BadManifest("schema_version missing".into()))?;
        if v > EMBEDDED_SCHEMA_VERSION {
            return Err(LoadError::SchemaTooNew {
                found: v,
                max: EMBEDDED_SCHEMA_VERSION,
            });
        }
    }

    let payload = payload.ok_or(LoadError::MissingPayload)?;
    Ok(parse_compact(&payload))
}

/// Parse the compact `vid:name,vid:name,...` format into a lookup table.
/// Mirror of `build_vendor_archive.py::parse_compact` — keep in sync.
fn parse_compact(s: &str) -> HashMap<u16, String> {
    let mut out = HashMap::new();
    for chunk in s.split(',') {
        if chunk.is_empty() {
            continue;
        }
        let Some((vid_hex, name_esc)) = chunk.split_once(':') else {
            continue;
        };
        let Ok(vid) = u16::from_str_radix(vid_hex, 16) else {
            continue;
        };
        out.insert(vid, unescape(name_esc));
    }
    out
}

fn unescape(s: &str) -> String {
    // Inverse of `_ESCAPE_RE` in build_vendor_archive.py. The producer only
    // ever emits ASCII `%XX` escapes (for `,` and `%`); we intentionally do
    // NOT decode multi-byte `%XX` runs here because that would require
    // assembling UTF-8 byte sequences and the producer never generates
    // them anyway — non-ASCII characters always pass through as raw UTF-8.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) =
                (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
            {
                let byte = hi * 16 + lo;
                if byte < 0x80 {
                    out.push(byte as char);
                    i += 3;
                    continue;
                }
                // 0x80..=0xFF: leave the `%XX` as a literal — see comment.
            }
        }
        // Step by one UTF-8 char so multi-byte sequences stay intact.
        let ch_start = i;
        let mut ch_end = i + 1;
        while ch_end < bytes.len() && (bytes[ch_end] & 0xC0) == 0x80 {
            ch_end += 1;
        }
        out.push_str(&s[ch_start..ch_end]);
        i = ch_end;
    }
    out
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_archive_loads_and_parses() {
        let n = embedded_vendor_count();
        assert!(
            n > 500,
            "embedded archive looks truncated: only {n} vendor entries"
        );
    }

    #[test]
    fn embedded_resolves_well_known_vids() {
        // These are the headline VIDs the curated overlay was created to
        // ensure — see issue FastLED/fbuild#718. If they vanish, the www
        // page's headline "what board is this VID:PID?" query degrades.
        // These need to be substrings the canonical upstream `usb.ids`
        // text database actually emits (since vendor-override mode does
        // not REPLACE names the upstream already has — see overlay
        // mode semantics in online-data-tools/overlay_usb_vid.py). VIDs
        // 0x303a and 0x2e8a are the ones the inlined supplement contributes.
        for (vid, expected_substr) in [
            (0x303a_u16, "Espressif"),                 // inlined supplement only
            (0x2e8a, "Raspberry Pi"),                  // inlined supplement only
            (0x0403, "Future Technology"),             // upstream
            (0x10c4, "Silicon Lab"),                   // upstream may say "Cygnal"
            (0x1a86, "QinHeng"),                       // upstream
            (0x16c0, "Van Ooijen Technische"),         // PJRC/Teensy via VOTI alloc
        ] {
            let name = vendor_name(vid).unwrap_or_else(|| {
                panic!("embedded archive missing vendor for VID 0x{vid:04X}")
            });
            assert!(
                name.to_lowercase().contains(&expected_substr.to_lowercase()),
                "VID 0x{vid:04X}: expected substring {expected_substr:?}, got {name:?}"
            );
        }
    }

    #[test]
    fn unknown_vid_returns_none() {
        // 0xBADD is in the unallocated portion of the USB-IF range as of
        // the 2026 snapshot. If a future archive picks it up the test
        // can move to another reserved range.
        assert!(vendor_name(0xBADD).is_none(),
                "0xBADD unexpectedly present: {:?}", vendor_name(0xBADD));
    }

    #[test]
    fn parse_compact_handles_escapes_and_unicode() {
        // The producer only ever escapes `,` and `%` (both ASCII). Non-ASCII
        // text passes through as raw UTF-8 — we verify both round-trip.
        let s = "0001:plain,0002:has%2Ccomma,0003:has%25percent,0004:em\u{2014}dash";
        let m = parse_compact(s);
        assert_eq!(m.get(&1).map(|s| s.as_str()), Some("plain"));
        assert_eq!(m.get(&2).map(|s| s.as_str()), Some("has,comma"));
        assert_eq!(m.get(&3).map(|s| s.as_str()), Some("has%percent"));
        let v = m.get(&4).expect("vid 4 missing");
        assert!(v.contains('—'), "missing em-dash: {v:?}");
    }
}
