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
//!   "reason": null
//! }
//! ```

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
    /// Why this build compiled from source, when the ladder did not serve it.
    ///
    /// `None` for a served build and for a build that never considered
    /// prebuilt at all (the hidden opt-in was off).
    #[serde(default)]
    pub reason: Option<String>,
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
            reason,
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
            reason: None,
        }
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
        assert!(value.get("reason").is_some_and(serde_json::Value::is_null));
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

        let served = info.with_prebuilt_libs(Some(PrebuiltLibsRecord::served(
            "https://example.invalid/core.tar.zst".to_string(),
            Some("arduino-esp32/3.3.12/esp32c3/fp-0123456789abcdef".to_string()),
            Some("c".repeat(64)),
        )));
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
    }

    #[test]
    fn a_declined_consult_keeps_its_reason() {
        let record = PrebuiltLibsRecord::source(Some("no published tree".to_string()));
        assert!(!record.used);
        assert_eq!(record.reason.as_deref(), Some("no published tree"));
    }
}
