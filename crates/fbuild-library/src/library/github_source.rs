//! GitHub `lib_deps` sources (FastLED/fbuild#2018).
//!
//! PlatformIO accepts a GitHub repository in `lib_deps` as
//! `https://github.com/<owner>/<repo>[.git][#<ref>]`, optionally prefixed
//! with `git+`. The `#<ref>` names a tag, branch or commit. fbuild installs
//! GitHub's source archive of that ref, `…/archive/<ref>.zip`, which GitHub
//! serves for all three; without a ref it fetches `archive/HEAD.zip`, the
//! repository's default branch, whatever it is called.
//!
//! The URL grammar is the shared PlatformIO one
//! ([`fbuild_core::platformio_package::parse_package_spec`]), and the
//! owner/repo split is the one platform repositories already use
//! ([`fbuild_packages_fetch::platformio_repository::github_owner_repo`]).

use std::io::Read;
use std::path::Path;

use fbuild_core::platformio_package::{PackageSource, parse_package_spec};
use fbuild_core::{FbuildError, Result};
use fbuild_packages_fetch::platformio_repository::github_owner_repo;
use sha2::{Digest, Sha256};

/// A GitHub repository pinned (or not) to one ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubSource {
    pub owner: String,
    pub repo: String,
    /// Tag, branch or commit after `#`; `None` is the default branch.
    pub reference: Option<String>,
}

impl GitHubSource {
    /// Parse a GitHub repository URL. `None` for anything else, including
    /// non-GitHub hosts, direct archive URLs and malformed refs.
    pub fn parse(spec: &str) -> Option<Self> {
        let parsed = parse_package_spec(&canonical_github_host(spec)).ok()?;
        if parsed.alias.is_some() {
            return None;
        }
        let PackageSource::Repository { url, reference } = parsed.source else {
            return None;
        };
        let (owner, repo) = github_owner_repo(&url).ok()?;
        if reference.as_deref().is_some_and(|r| !valid_reference(r)) {
            return None;
        }
        Some(Self {
            owner,
            repo,
            reference,
        })
    }

    /// The repository URL without `.git` or the ref.
    pub fn repo_url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.repo)
    }

    /// GitHub's zip archive of the ref (`HEAD` when there is none). Each ref
    /// path segment is percent-encoded; a `/` inside a branch name stays a
    /// path separator, which is how GitHub expects it.
    pub fn archive_url(&self) -> String {
        let reference = self.reference.as_deref().unwrap_or("HEAD");
        let (last, dirs) = reference
            .rsplit_once('/')
            .map_or((reference, ""), |(dirs, last)| (last, dirs));
        let mut url = reqwest::Url::parse("https://github.com/").expect("static URL parses");
        {
            let mut segments = url
                .path_segments_mut()
                .expect("https URL has path segments");
            segments.pop_if_empty();
            segments.extend([self.owner.as_str(), self.repo.as_str(), "archive"]);
            if !dirs.is_empty() {
                segments.extend(dirs.split('/'));
            }
            segments.push(&format!("{last}.zip"));
        }
        url.into()
    }

    /// Filesystem-safe tag for the ref, so two refs of one repository never
    /// share an install directory. `None` without a ref. A ref that needed
    /// characters replaced gets a short digest, so `a/b` and `a_b` differ.
    pub fn dir_suffix(&self) -> Option<String> {
        let reference = self.reference.as_deref()?;
        let safe: String = reference
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if safe == reference {
            return Some(safe);
        }
        let digest = Sha256::digest(reference.as_bytes());
        let short: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
        Some(format!("{safe}-{short}"))
    }
}

/// `http://` and `www.` spellings of github.com name the same repository
/// (and `fbuild sync` classifies them as GitHub too); spell them as the
/// canonical `https://github.com/` the shared parser expects.
fn canonical_github_host(spec: &str) -> String {
    let (prefix, rest) = spec
        .strip_prefix("git+")
        .map_or(("", spec), |rest| ("git+", rest));
    for host in [
        "http://github.com/",
        "http://www.github.com/",
        "https://www.github.com/",
    ] {
        if let Some(tail) = rest.strip_prefix(host) {
            return format!("{prefix}https://github.com/{tail}");
        }
    }
    spec.to_string()
}

/// A ref must be a usable URL path: no whitespace or control characters, no
/// `?`, and no empty, `.` or `..` path segments.
fn valid_reference(reference: &str) -> bool {
    !reference.is_empty()
        && !reference
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '?')
        && reference
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
}

/// Fail with a clear message when `path`, downloaded from `url`, is not a
/// zip archive -- e.g. the HTML page GitHub answers a bad URL with -- rather
/// than letting the zip reader report a missing end-of-central-directory.
pub fn ensure_zip_archive(path: &Path, url: &str) -> Result<()> {
    let mut head = Vec::with_capacity(512);
    std::fs::File::open(path)
        .and_then(|file| file.take(512).read_to_end(&mut head))
        .map_err(|e| {
            FbuildError::PackageError(format!(
                "failed to read downloaded archive {}: {e}",
                path.display()
            ))
        })?;
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    let got = if text.contains("<!doctype html") || text.contains("<html") {
        "an HTML page"
    } else if head.is_empty() {
        "an empty response"
    } else {
        "data that is not a zip archive"
    };
    Err(FbuildError::PackageError(format!(
        "{url} returned {got}, not a zip archive; check that the repository and ref exist"
    )))
}

#[cfg(test)]
#[path = "github_source_tests.rs"]
mod tests;
