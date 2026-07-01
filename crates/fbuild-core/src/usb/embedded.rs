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
    for entry in archive
        .entries()
        .map_err(|e| LoadError::Tar(e.to_string()))?
    {
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
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
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
            (0x303a_u16, "Espressif"),         // inlined supplement only
            (0x2e8a, "Raspberry Pi"),          // inlined supplement only
            (0x0403, "Future Technology"),     // upstream
            (0x10c4, "Silicon Lab"),           // upstream may say "Cygnal"
            (0x1a86, "QinHeng"),               // upstream
            (0x16c0, "Van Ooijen Technische"), // PJRC/Teensy via VOTI alloc
        ] {
            let name = vendor_name(vid)
                .unwrap_or_else(|| panic!("embedded archive missing vendor for VID 0x{vid:04X}"));
            assert!(
                name.to_lowercase()
                    .contains(&expected_substr.to_lowercase()),
                "VID 0x{vid:04X}: expected substring {expected_substr:?}, got {name:?}"
            );
        }
    }

    #[test]
    fn embedded_resolves_every_issue_740_vendor() {
        // FastLED/fbuild#740 hand-verified 19 vendor VIDs at each
        // `online-data` publish. Every prior verification pass has been
        // a manual `gh` + `jq` sweep against the published JSON.
        //
        // Codify the entire table here so CI catches any regression the
        // moment the embedded vendor archive is rebuilt without one of
        // the headline VIDs — the exact class of drift that previously
        // required manually re-running the verification each cycle.
        //
        // Match is case-insensitive substring; ANY alternative matches.
        // ALL failures are collected before asserting so regressions
        // surface as a single message with every affected row.
        //
        // ## Overlay vs embedded drift (documented, not aspirational)
        //
        // The #740 issue body's "Vendor-resolution results" table was
        // taken from the PUBLISHED `online-data/data/usb-vid.json`,
        // which has `vendor_names_inlined.py` applied via
        // `overlay_usb_vid.py --mode vendor-override`. Three of the 19
        // VIDs in that table were vendor-name-overridden by that
        // overlay pass (so the "Actual" column reads the overlay
        // value):
        //
        //   - 0x045B — upstream `usb.ids` says "Hitachi, Ltd";
        //     overlay renames to "Renesas Electronics".
        //   - 0x2A03 — upstream says "dog hunter AG" (the actual
        //     VID holder); overlay renames to "Arduino LLC" (the
        //     downstream licensee that ships boards under this VID).
        //   - 0x2544 — missing from upstream entirely; overlay adds
        //     as "Silicon Labs" (a.k.a. Energy Micro).
        //
        // The embedded archive shipped in `data/usb-vendors.tar.zst`
        // is built from the OVERLAID JSON, so it SHOULD also carry
        // these overrides. Whether it does today is a snapshot of the
        // last archive rebuild — this test asserts the current
        // effective label, and the substring list carries BOTH the
        // upstream and overlay names so the test survives either
        // resolution outcome without silently accepting drift.
        let rows: &[(u16, &[&str])] = &[
            (0x303a, &["Espressif"]),
            (0x2e8a, &["Raspberry Pi"]),
            (0x0483, &["STMicroelectronics", "STMicro"]),
            (0x1fc9, &["NXP"]),
            (0x1915, &["Nordic"]),
            (0x03eb, &["Atmel"]),
            (0x04d8, &["Microchip"]),
            (0x10c4, &["Silicon Lab", "Cygnal"]),
            (0x1a86, &["QinHeng", "WCH"]),
            (0x0403, &["Future Technology", "FTDI"]),
            // 0x1cbe is Luminary Micro (Cortex-M / Apollo3 bootloader
            // VID reused by Sparkfun Artemis products in the field).
            (0x1cbe, &["Luminary", "Apollo3", "Sparkfun"]),
            (0x2341, &["Arduino"]),
            (0x239a, &["Adafruit"]),
            (0x1b4f, &["SparkFun", "Spark Fun"]),
            (0x16c0, &["Van Ooijen", "PJRC"]),
            (0x2886, &["Seeed"]),
            // Overlay-covered VIDs — either name is accepted so the
            // test survives an archive rebuild + overlay pipeline
            // change in either direction. See the module comment
            // above for the source of each alternative.
            (0x045b, &["Renesas", "Hitachi"]),
            (0x2a03, &["Arduino", "dog hunter"]),
        ];

        // 0x2544 (Silicon Labs) is a supplement-only VID — the archive
        // MAY not carry it, depending on whether the archive was
        // rebuilt after the vendor_names_inlined.py addition. Assert
        // that IF present it resolves to Silicon Labs; a missing entry
        // is tolerated but reported so the drift is visible in test
        // logs.
        let overlay_only: &[(u16, &[&str])] = &[(0x2544, &["Silicon Lab", "Cygnal"])];

        let mut failures = Vec::new();
        for (vid, expected_alts) in rows {
            match vendor_name(*vid) {
                None => failures.push(format!(
                    "VID 0x{vid:04X}: missing from embedded archive; expected any of {expected_alts:?}"
                )),
                Some(name) => {
                    let name_lc = name.to_lowercase();
                    let matched = expected_alts
                        .iter()
                        .any(|alt| name_lc.contains(&alt.to_lowercase()));
                    if !matched {
                        failures.push(format!(
                            "VID 0x{vid:04X}: got {name:?}, expected any-of {expected_alts:?}"
                        ));
                    }
                }
            }
        }

        // Overlay-only rows: report drift, but don't fail. Missing =
        // "archive hasn't been rebuilt with the current overlay yet";
        // wrong name = "archive was rebuilt but overlay changed the
        // canonical string" — either way a follow-up rebuild is the
        // remediation, not a source-code fix.
        for (vid, expected_alts) in overlay_only {
            match vendor_name(*vid) {
                None => eprintln!(
                    "note: VID 0x{vid:04X} not in embedded archive \
                     (overlay-only; archive rebuild will pick it up); \
                     expected any of {expected_alts:?}"
                ),
                Some(name) => {
                    let name_lc = name.to_lowercase();
                    let matched = expected_alts
                        .iter()
                        .any(|alt| name_lc.contains(&alt.to_lowercase()));
                    if !matched {
                        failures.push(format!(
                            "VID 0x{vid:04X}: got {name:?}, expected any-of {expected_alts:?}"
                        ));
                    }
                }
            }
        }

        assert!(
            failures.is_empty(),
            "FastLED/fbuild#740 vendor-VID table drift ({} row(s) failed):\n  {}",
            failures.len(),
            failures.join("\n  "),
        );
    }

    #[test]
    fn unknown_vid_returns_none() {
        // 0xBADD is in the unallocated portion of the USB-IF range as of
        // the 2026 snapshot. If a future archive picks it up the test
        // can move to another reserved range.
        assert!(
            vendor_name(0xBADD).is_none(),
            "0xBADD unexpectedly present: {:?}",
            vendor_name(0xBADD)
        );
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
