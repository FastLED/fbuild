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

    /// Parse a `.lnk` file in either supported format.
    ///
    /// Two formats exist in the wild and both are accepted:
    ///
    /// - **JSON** (canonical, what `fbuild lnk add` writes) — see
    ///   [`Self::from_json_str`].
    /// - **Text**, the format FastLED's C++ runtime parses
    ///   (`fl::parse_lnk_with_metadata()`): `#` comments and blank lines are
    ///   skipped, the first remaining line is the URL, and subsequent
    ///   `key=value` lines carry metadata. Unknown keys are ignored so the
    ///   format can grow.
    ///
    /// The format is detected by sniffing the first non-blank, non-comment
    /// character: `{` means JSON, anything else is text.
    pub fn from_str_any(s: &str) -> Result<Self> {
        if first_meaningful_char(s) == Some('{') {
            Self::from_json_str(s)
        } else {
            Self::from_text_str(s)
        }
    }

    /// Parse the plain-text `.lnk` form.
    ///
    /// `sha256` is required here just as it is for JSON: the resolver caches
    /// by content digest, so a `.lnk` without one cannot be content-addressed
    /// or verified. The text format treats it as optional, so a file written
    /// for the C++ runtime may need a digest added before fbuild can use it —
    /// the error says exactly that.
    pub fn from_text_str(s: &str) -> Result<Self> {
        let mut url: Option<String> = None;
        let mut sha256: Option<String> = None;

        for raw_line in s.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if url.is_none() {
                url = Some(line.to_string());
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                // Unknown non-key=value line: ignore for forward-compat,
                // matching the C++ and Python parsers.
                continue;
            };
            if key.trim() == "sha256" {
                sha256 = Some(value.trim().to_string());
            }
            // Other keys (e.g. `fallback`) are recognized by the runtime but
            // have no representation in LnkFile yet; ignored rather than fatal.
        }

        let Some(url) = url else {
            return Err(FbuildError::PackageError(
                "no URL found in .lnk (expected a non-comment line with the asset URL)".to_string(),
            ));
        };
        let Some(sha256) = sha256 else {
            return Err(FbuildError::PackageError(format!(
                "text-format .lnk for {url} has no `sha256=` line; fbuild caches by content \
                 digest and cannot verify or content-address without one. Add a \
                 `sha256=<64 hex chars>` line, or regenerate the file with `fbuild lnk add <url>`."
            )));
        };

        Self::from_raw(LnkFileRaw {
            v: 1,
            url,
            sha256,
            size: None,
            extract: None,
        })
    }

    /// Parse a `.lnk` file from disk.
    pub fn from_path(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| {
            FbuildError::PackageError(format!("failed to read .lnk file {}: {e}", path.display()))
        })?;
        let s = std::str::from_utf8(&bytes).map_err(|_| {
            FbuildError::PackageError(format!(".lnk file {} is not valid UTF-8", path.display()))
        })?;
        Self::from_str_any(s).map_err(|e| match e {
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

/// First character that is neither whitespace nor part of a `#` comment line.
/// Used to tell the JSON form from the text form.
fn first_meaningful_char(s: &str) -> Option<char> {
    s.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .and_then(|line| line.chars().next())
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
    fn parses_text_format_with_sha256() {
        let text = format!(
            "# .lnk asset link file\n\
             # comments are skipped\n\
             \n\
             https://example.com/track.mp3\n\
             sha256={VALID_SHA}\n"
        );
        let lnk = LnkFile::from_str_any(&text).unwrap();
        assert_eq!(lnk.version, 1);
        assert_eq!(lnk.url, "https://example.com/track.mp3");
        assert_eq!(lnk.sha256, VALID_SHA);
        assert_eq!(lnk.extract, ExtractMode::File);
    }

    #[test]
    fn text_format_ignores_unknown_keys() {
        let text = format!(
            "https://example.com/x.bin\nsha256={VALID_SHA}\nfallback=https://mirror/x.bin\nfuture=1\n"
        );
        let lnk = LnkFile::from_str_any(&text).unwrap();
        assert_eq!(lnk.url, "https://example.com/x.bin");
    }

    #[test]
    fn text_format_without_sha256_explains_why_it_is_required() {
        let text = "https://example.com/track.mp3\n";
        let err = LnkFile::from_str_any(text).unwrap_err().to_string();
        assert!(err.contains("sha256"), "unexpected error: {err}");
        assert!(err.contains("fbuild lnk add"), "unexpected error: {err}");
    }

    #[test]
    fn text_format_without_url_is_rejected() {
        let err = LnkFile::from_str_any("# only a comment\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no URL"), "unexpected error: {err}");
    }

    #[test]
    fn sniffer_routes_leading_whitespace_to_json() {
        let json = format!(
            "\n  {{\"v\":1,\"url\":\"https://example.com/x.bin\",\"sha256\":\"{VALID_SHA}\"}}"
        );
        let lnk = LnkFile::from_str_any(&json).unwrap();
        assert_eq!(lnk.url, "https://example.com/x.bin");
    }

    #[test]
    fn json_preceded_by_a_hash_comment_is_rejected_as_json() {
        // JSON has no comment syntax, so `#` before `{` is malformed rather
        // than a mixed-format file. The sniffer still routes it to the JSON
        // branch (first meaningful char is `{`), and serde reports the error.
        let json = format!(
            "# not legal in JSON\n{{\"v\":1,\"url\":\"https://example.com/x.bin\",\"sha256\":\"{VALID_SHA}\"}}"
        );
        let err = LnkFile::from_str_any(&json).unwrap_err().to_string();
        assert!(err.contains("invalid .lnk JSON"), "unexpected error: {err}");
    }

    #[test]
    fn json_and_text_forms_agree() {
        let json = valid_minimal();
        let text = format!("https://example.com/x.bin\nsha256={VALID_SHA}\n");
        assert_eq!(
            LnkFile::from_str_any(&json).unwrap(),
            LnkFile::from_str_any(&text).unwrap()
        );
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
