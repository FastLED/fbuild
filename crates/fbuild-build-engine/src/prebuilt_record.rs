//! The `prebuilt_libs` block of `build_info_<env>.json` (FastLED/fbuild#2196
//! cell 4).
//!
//! `#1882`/`#2196` gave the build an opt-in exact-fingerprint serving ladder
//! for framework artifacts. Decision 1 of
//! `docs/design/artifact-serving-decisions.md` requires every build to record
//! which artifact it actually linked against, so a firmware can be traced back
//! to the archive or tree behind it — and so the `#1882` cold/warm benchmark
//! can tell its two arms apart from the build output alone.
//!
//! The block is **always** emitted: a build that compiled from source records
//! `used: false` rather than omitting the key, because a missing key cannot be
//! distinguished from metadata written by an older fbuild.
//!
//! ```json
//! "prebuilt_libs": {
//!   "used": true,
//!   "source": "https://example.invalid/v1/.../core.tar.zst",
//!   "key": "arduino-esp32/3.3.12/esp32/fp-0123456789abcdef",
//!   "sha256": "0123…",
//!   "fingerprint": "0123…",
//!   "reason": null,
//!   "archive_bytes": 4075144,
//!   "downloaded": true
//! }
//! ```
//!
//! `archive_bytes` and `downloaded` are the download accounting decision 6 of
//! `docs/design/artifact-serving-decisions.md` asks the benchmark to report: the
//! published archive's size, and whether this build actually transferred it
//! (a tree reused from the local cache downloads nothing, so its
//! `bytes_downloaded` is zero even though `archive_bytes` is not). Both stay
//! `null` for a source build — the control arm's bytes come from the framework
//! bundle, which is not a prebuilt artifact.

use serde::{Deserialize, Serialize};

/// What the prebuilt serving ladder decided for one build.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrebuiltLibsRecord {
    /// Whether this build linked a served prebuilt artifact.
    pub used: bool,
    /// The URL the artifact was fetched from, or `"source"` for a source build.
    pub source: String,
    /// The published artifact's identity key, when one was served.
    #[serde(default)]
    pub key: Option<String>,
    /// SHA-256 of the served archive, when one was served.
    #[serde(default)]
    pub sha256: Option<String>,
    /// The exact fingerprint the ladder asked for, when the framework had an
    /// identity.
    ///
    /// [`Self::key`] carries only `fp-<sha16>` — that is the bucket and
    /// extraction path, not the address a publisher must serve: a catalog
    /// entry is keyed by the whole value, advertised as
    /// `fbuild-fp:<fingerprint>`. Recording it here is what lets a build's own
    /// output name the object a publisher would have to upload, which is the
    /// first step of publishing a tree this build could then hydrate.
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// Why this build compiled from source, when the ladder did not serve it.
    ///
    /// `None` for a served build and for a build that never considered
    /// prebuilt at all (the hidden opt-in was off).
    #[serde(default)]
    pub reason: Option<String>,
    /// Size in bytes of the published archive behind the served tree.
    ///
    /// Decision 6's `bytes_downloaded`: the descriptor pins this number, so a
    /// served build reports the transfer it would cost without a network
    /// counter. `None` for a source build, which downloads no prebuilt archive.
    #[serde(default)]
    pub archive_bytes: Option<u64>,
    /// Whether this build transferred the archive, or reused a tree already in
    /// the local cache. `None` for a source build.
    ///
    /// A reuse still serves the build, but its `bytes_downloaded` is zero; the
    /// benchmark must average the two cases apart or a warm cache reads as a
    /// smaller artifact.
    #[serde(default)]
    pub downloaded: Option<bool>,
}

impl PrebuiltLibsRecord {
    /// The record for a build that compiled everything from source.
    ///
    /// `reason` is `Some` only when the ladder was actually consulted, so
    /// "never asked" and "asked and declined" stay distinguishable.
    #[must_use]
    pub fn source(reason: Option<String>) -> Self {
        Self {
            used: false,
            source: "source".to_string(),
            key: None,
            sha256: None,
            fingerprint: None,
            reason,
            archive_bytes: None,
            downloaded: None,
        }
    }

    /// The record for a build that linked a served prebuilt artifact.
    #[must_use]
    pub fn served(source: String, key: Option<String>, sha256: Option<String>) -> Self {
        Self {
            used: true,
            source,
            key,
            sha256,
            fingerprint: None,
            reason: None,
            archive_bytes: None,
            downloaded: None,
        }
    }

    /// Record the exact fingerprint this build asked the ladder for.
    ///
    /// Separate from the constructors because it is orthogonal to how the
    /// consult ended: a source build that missed the catalog needs it just as
    /// much as a served one, since it names what a publisher would have to
    /// upload for the next build of the same cell to hit.
    #[must_use]
    pub fn with_fingerprint(mut self, fingerprint: Option<String>) -> Self {
        self.fingerprint = fingerprint;
        self
    }

    /// Record the download accounting for a served tree: the archive's pinned
    /// size and whether this build transferred it (decision 6's
    /// `bytes_downloaded`, per arm).
    #[must_use]
    pub fn with_download(mut self, archive_bytes: Option<u64>, downloaded: Option<bool>) -> Self {
        self.archive_bytes = archive_bytes;
        self.downloaded = downloaded;
        self
    }
}

impl Default for PrebuiltLibsRecord {
    fn default() -> Self {
        Self::source(None)
    }
}

#[cfg(test)]
mod tests {
    use super::PrebuiltLibsRecord;

    #[test]
    fn default_records_a_source_build_with_all_four_fields() {
        let value = serde_json::to_value(PrebuiltLibsRecord::default()).unwrap();
        assert_eq!(value["used"], serde_json::json!(false));
        assert_eq!(value["source"], serde_json::json!("source"));
        // Present as JSON null, never omitted: a missing key would be
        // indistinguishable from metadata written by an older fbuild.
        assert!(value.get("key").is_some_and(serde_json::Value::is_null));
        assert!(value.get("sha256").is_some_and(serde_json::Value::is_null));
        assert!(
            value
                .get("fingerprint")
                .is_some_and(serde_json::Value::is_null)
        );
        assert!(value.get("reason").is_some_and(serde_json::Value::is_null));
        // Decision 6's download accounting: absent for a source build, but
        // PRESENT as null so an older fbuild's metadata is distinguishable.
        assert!(
            value
                .get("archive_bytes")
                .is_some_and(serde_json::Value::is_null)
        );
        assert!(
            value
                .get("downloaded")
                .is_some_and(serde_json::Value::is_null)
        );
    }

    #[test]
    fn served_record_names_the_artifact_it_linked() {
        let record = PrebuiltLibsRecord::served(
            "https://example.invalid/v1/core.tar.zst".to_string(),
            Some("arduino-esp32/3.3.12/esp32/fp-0123456789abcdef".to_string()),
            Some("a".repeat(64)),
        );
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["used"], serde_json::json!(true));
        assert_eq!(
            value["key"],
            serde_json::json!("arduino-esp32/3.3.12/esp32/fp-0123456789abcdef")
        );
        assert_eq!(value["sha256"], serde_json::json!("a".repeat(64)));
        assert!(value.get("reason").is_some_and(serde_json::Value::is_null));
    }

    /// Decision 6's `bytes_downloaded` per arm: a served build that had to
    /// fetch reports the archive's pinned size; a served build that reused a
    /// local tree reports the same size AND `downloaded: false`, so the bench
    /// can tell a transfer from a cache hit instead of averaging them.
    #[test]
    fn a_served_record_carries_its_download_accounting() {
        let fetched = PrebuiltLibsRecord::served(
            "https://example.invalid/v1/core.tar.zst".to_string(),
            Some("arduino-esp32/3.3.12/esp32/fp-0123456789abcdef".to_string()),
            Some("a".repeat(64)),
        )
        .with_download(Some(4_075_144), Some(true));
        let value = serde_json::to_value(&fetched).unwrap();
        assert_eq!(value["archive_bytes"], serde_json::json!(4_075_144_u64));
        assert_eq!(value["downloaded"], serde_json::json!(true));

        let reused = fetched.clone().with_download(Some(4_075_144), Some(false));
        let value = serde_json::to_value(&reused).unwrap();
        assert_eq!(value["archive_bytes"], serde_json::json!(4_075_144_u64));
        assert_eq!(
            value["downloaded"],
            serde_json::json!(false),
            "a reuse downloads nothing and must say so"
        );
    }

    /// A source build downloads no prebuilt archive, so the accounting stays
    /// null even when the caller asks for it: the control arm's bytes are the
    /// framework bundle's, not this artifact's.
    #[test]
    fn a_source_record_has_no_download_accounting() {
        let record = PrebuiltLibsRecord::source(Some("no published tree".to_string()));
        assert_eq!(record.archive_bytes, None);
        assert_eq!(record.downloaded, None);
    }

    /// The block actually lands in `build_info_<env>.json` — through the real
    /// emitter, not just serde on the record.
    #[test]
    fn the_emitted_build_info_carries_the_block() {
        use crate::build_info::{BuildInfo, emit_build_info};
        let tmp = tempfile::TempDir::new().unwrap();
        let info = BuildInfo::new(
            std::path::Path::new("/bin/firmware.elf"),
            None,
            None,
            None,
            None,
            std::path::Path::new("/bin/xtensa-esp32-elf-size"),
            vec![],
            vec![],
            vec![],
            vec![],
            "espressif32".to_string(),
            "esp32c3".to_string(),
            "esp32c3".to_string(),
        );
        // A platform that never consults the ladder keeps its JSON unchanged.
        emit_build_info(tmp.path(), "esp32c3", &info).unwrap();
        let bytes = std::fs::read(tmp.path().join("build_info_esp32c3.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            parsed["esp32c3"].get("prebuilt_libs").is_none(),
            "no consult is no key: {parsed}"
        );

        let served = info.with_prebuilt_libs(Some(
            PrebuiltLibsRecord::served(
                "https://example.invalid/core.tar.zst".to_string(),
                Some("arduino-esp32/3.3.12/esp32c3/fp-0123456789abcdef".to_string()),
                Some("c".repeat(64)),
            )
            .with_fingerprint(Some("d".repeat(64))),
        ));
        emit_build_info(tmp.path(), "esp32c3", &served).unwrap();
        let bytes = std::fs::read(tmp.path().join("build_info_esp32c3.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            parsed["esp32c3"]["prebuilt_libs"]["used"],
            serde_json::json!(true)
        );
        assert_eq!(
            parsed["esp32c3"]["prebuilt_libs"]["key"],
            serde_json::json!("arduino-esp32/3.3.12/esp32c3/fp-0123456789abcdef")
        );
        assert_eq!(
            parsed["esp32c3"]["prebuilt_libs"]["sha256"],
            serde_json::json!("c".repeat(64))
        );
        // The full fingerprint reaches the file: `key` stops at its sha16
        // prefix, so this is the only place a publisher can read the address.
        assert_eq!(
            parsed["esp32c3"]["prebuilt_libs"]["fingerprint"],
            serde_json::json!("d".repeat(64))
        );
    }

    #[test]
    fn a_declined_consult_keeps_its_reason() {
        let record = PrebuiltLibsRecord::source(Some("no published tree".to_string()));
        assert!(!record.used);
        assert_eq!(record.reason.as_deref(), Some("no published tree"));
    }
}
