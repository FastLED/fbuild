//! `.lnk` file format: JSON pointer to a remotely-hosted binary blob.
//!
//! A `.lnk` file is a small JSON manifest checked into source control that
//! points at a binary asset hosted somewhere reachable over HTTP. At build
//! time, fbuild reads the manifest, fetches the blob (with sha256
//! verification), caches it locally, and materializes it next to the
//! `.lnk` (in the build tree, not the source tree) so downstream build
//! steps can consume it as a normal file.
//!
//! ## Schema (v1)
//!
//! ```json
//! {
//!   "v": 1,
//!   "url": "https://example.com/path/to/asset.bin",
//!   "sha256": "abcdef0123...64-hex-chars...",
//!   "size": 1234567,
//!   "extract": "file"
//! }
//! ```
//!
//! Required fields: `v`, `url`, `sha256`. Optional: `size` (advisory; used
//! for early-fail before fetching huge blobs), `extract` (default `"file"`;
//! `"zip"` and `"tar.gz"` extract into a directory tree at the resolved
//! path).
//!
//! ## Why JSON
//!
//! - human-readable + git-diff-able
//! - no external tooling needed (vs Git LFS pointer format)
//! - sha256 is mandatory → reproducible builds + content-addressable cache
//! - extensible via `v` field if a v2 ever happens

use std::path::Path;

use fbuild_core::{FbuildError, Result};
use serde::{Deserialize, Serialize};

/// How a fetched blob should be materialized into the build tree.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtractMode {
    /// Materialize the blob as a single file (default).
    #[default]
    File,
    /// Treat the blob as a zip archive; extract into a directory.
    Zip,
    /// Treat the blob as a `.tar.gz`; extract into a directory.
    #[serde(rename = "tar.gz")]
    TarGz,
}

/// In-memory representation of a parsed `.lnk` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LnkFile {
    /// Format version. Currently always 1.
    pub version: u32,
    /// URL to fetch the blob from. Must be `http://` or `https://`.
    pub url: String,
    /// SHA-256 of the expected blob content, lowercase hex (64 chars).
    pub sha256: String,
    /// Optional advisory size in bytes. Used to refuse oversized blobs
    /// before the fetch starts.
    pub size: Option<u64>,
    /// How the blob should be materialized.
    pub extract: ExtractMode,
}

/// Raw on-disk JSON representation. Kept private so we can validate fields
/// after deserialization and surface a single canonical `LnkFile` to callers.
#[derive(Debug, Deserialize)]
struct LnkFileRaw {
    v: u32,
    url: String,
    sha256: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    extract: Option<ExtractMode>,
}

impl LnkFile {
    /// Parse a `.lnk` file from a JSON string. Validates schema version,
    /// URL scheme, and sha256 format. Named `from_json_str` (rather
    /// than `from_str`) so it doesn't shadow the `std::str::FromStr`
    /// trait method — a plain `.lnk` file is always JSON, so the name
    /// signals the format explicitly.
    pub fn from_json_str(s: &str) -> Result<Self> {
        let raw: LnkFileRaw = serde_json::from_str(s)
            .map_err(|e| FbuildError::PackageError(format!("invalid .lnk JSON: {e}")))?;
        Self::from_raw(raw)
    }

    /// Parse a `.lnk` file from disk.
    pub fn from_path(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| {
            FbuildError::PackageError(format!("failed to read .lnk file {}: {e}", path.display()))
        })?;
        let s = std::str::from_utf8(&bytes).map_err(|_| {
            FbuildError::PackageError(format!(".lnk file {} is not valid UTF-8", path.display()))
        })?;
        Self::from_json_str(s).map_err(|e| match e {
            FbuildError::PackageError(msg) => {
                FbuildError::PackageError(format!("{}: {msg}", path.display()))
            }
            other => other,
        })
    }

    fn from_raw(raw: LnkFileRaw) -> Result<Self> {
        if raw.v != 1 {
            return Err(FbuildError::PackageError(format!(
                "unsupported .lnk schema version {} (only v=1 is supported)",
                raw.v
            )));
        }
        if !raw.url.starts_with("http://") && !raw.url.starts_with("https://") {
            return Err(FbuildError::PackageError(format!(
                "url must start with http:// or https://, got `{}`",
                raw.url
            )));
        }
        validate_sha256_hex(&raw.sha256)?;
        Ok(Self {
            version: raw.v,
            url: raw.url,
            sha256: raw.sha256.to_ascii_lowercase(),
            size: raw.size,
            extract: raw.extract.unwrap_or_default(),
        })
    }
}

fn validate_sha256_hex(s: &str) -> Result<()> {
    if s.len() != 64 {
        return Err(FbuildError::PackageError(format!(
            "sha256 must be 64 hex chars, got {} chars",
            s.len()
        )));
    }
    if !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(FbuildError::PackageError(
            "sha256 contains non-hex characters".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_SHA: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    fn valid_minimal() -> String {
        format!(r#"{{"v":1,"url":"https://example.com/x.bin","sha256":"{VALID_SHA}"}}"#)
    }

    #[test]
    fn parses_minimal_valid() {
        let lnk = LnkFile::from_json_str(&valid_minimal()).unwrap();
        assert_eq!(lnk.version, 1);
        assert_eq!(lnk.url, "https://example.com/x.bin");
        assert_eq!(lnk.sha256, VALID_SHA);
        assert_eq!(lnk.size, None);
        assert_eq!(lnk.extract, ExtractMode::File);
    }

    #[test]
    fn parses_full_valid() {
        let json = format!(
            r#"{{"v":1,"url":"https://example.com/x.zip","sha256":"{VALID_SHA}","size":42,"extract":"zip"}}"#
        );
        let lnk = LnkFile::from_json_str(&json).unwrap();
        assert_eq!(lnk.size, Some(42));
        assert_eq!(lnk.extract, ExtractMode::Zip);
    }

    #[test]
    fn parses_tar_gz() {
        let json = format!(
            r#"{{"v":1,"url":"https://x/y.tgz","sha256":"{VALID_SHA}","extract":"tar.gz"}}"#
        );
        let lnk = LnkFile::from_json_str(&json).unwrap();
        assert_eq!(lnk.extract, ExtractMode::TarGz);
    }

    #[test]
    fn rejects_unsupported_version() {
        let json = format!(r#"{{"v":2,"url":"https://x/y.bin","sha256":"{VALID_SHA}"}}"#);
        let err = LnkFile::from_json_str(&json).unwrap_err().to_string();
        assert!(
            err.contains("unsupported .lnk schema version 2"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_non_http_scheme() {
        let json = format!(r#"{{"v":1,"url":"ftp://x/y.bin","sha256":"{VALID_SHA}"}}"#);
        let err = LnkFile::from_json_str(&json).unwrap_err().to_string();
        assert!(err.contains("must start with http"), "got: {err}");
    }

    #[test]
    fn rejects_short_sha256() {
        let json = r#"{"v":1,"url":"https://x/y.bin","sha256":"abc"}"#;
        let err = LnkFile::from_json_str(json).unwrap_err().to_string();
        assert!(err.contains("64 hex chars"), "got: {err}");
    }

    #[test]
    fn rejects_non_hex_sha256() {
        let nonhex = "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ";
        let json = format!(r#"{{"v":1,"url":"https://x/y.bin","sha256":"{nonhex}"}}"#);
        let err = LnkFile::from_json_str(&json).unwrap_err().to_string();
        assert!(err.contains("non-hex"), "got: {err}");
    }

    #[test]
    fn rejects_missing_required_field() {
        let json = r#"{"v":1,"url":"https://x/y.bin"}"#;
        // missing sha256
        let err = LnkFile::from_json_str(json).unwrap_err().to_string();
        assert!(err.contains("invalid .lnk JSON"), "got: {err}");
    }

    #[test]
    fn rejects_malformed_json() {
        let err = LnkFile::from_json_str("{not json}")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid .lnk JSON"), "got: {err}");
    }

    #[test]
    fn lowercases_sha256() {
        let upper = VALID_SHA.to_ascii_uppercase();
        let json = format!(r#"{{"v":1,"url":"https://x/y.bin","sha256":"{upper}"}}"#);
        let lnk = LnkFile::from_json_str(&json).unwrap();
        assert_eq!(lnk.sha256, VALID_SHA);
    }

    #[test]
    fn from_path_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.bin.lnk");
        std::fs::write(&path, valid_minimal()).unwrap();
        let lnk = LnkFile::from_path(&path).unwrap();
        assert_eq!(lnk.url, "https://example.com/x.bin");
    }

    #[test]
    fn from_path_includes_path_in_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.lnk");
        std::fs::write(&path, "{nope}").unwrap();
        let err = LnkFile::from_path(&path).unwrap_err().to_string();
        assert!(err.contains("bad.lnk"), "got: {err}");
    }
}
