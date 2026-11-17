//! GitHub `lib_deps` sources (FastLED/fbuild#2018).
//!
//! PlatformIO accepts a GitHub repository in `lib_deps` as
//! `https://github.com/<owner>/<repo>[.git][#<ref>]`, optionally prefixed
//! with `git+`. The `#<ref>` names a tag, branch or commit. fbuild installs
//! GitHub's source archive of that ref, `…/archive/<ref>.zip`, which GitHub
//! serves for all three; without a ref it fetches `archive/HEAD.zip`, the
//! repository's default branch, whatever it is called.
//!
//! GitHub's web URL of a ref (`…/tree/<ref>`) and its direct archive URLs
//! (`…/archive/<ref>.zip`, `…/archive/refs/tags/<tag>.zip`, `.tar.gz`) name
//! one ref too, and install the same way. PlatformIO itself clones a
//! `/tree/` URL with git, which fails, and downloads an archive URL as a
//! plain file (FastLED/fbuild#2042).
//!
//! Like PlatformIO, an install is pinned: a URL without a ref downloads the
//! default branch once and reuses it, without a network call, until the
//! build directory is cleaned (`fbuild clean`, or `fbuild build --clean`).
//! PlatformIO likewise reinstalls only on `pio pkg update` (FastLED/fbuild#2042).
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
        let spec = canonical_github_host(spec);
        if let Some(web) = Self::parse_web_url(&spec) {
            return web;
        }
        let parsed = parse_package_spec(&spec).ok()?;
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

    /// GitHub's web URL of a ref, `…/<o>/<r>/tree/<ref>`, or a direct
    /// archive URL, `…/<o>/<r>/archive/<ref>.zip|.tar.gz` (including the
    /// `refs/tags/<t>` and `refs/heads/<b>` forms). Both name one ref, so
    /// they install like `#<ref>` (FastLED/fbuild#2042). `None` when the URL
    /// has neither shape; `Some(None)` when it has one but is malformed.
    fn parse_web_url(spec: &str) -> Option<Option<Self>> {
        let path = spec.strip_prefix("https://github.com/")?;
        let mut segments = path.splitn(4, '/');
        let (owner, repo, kind) = (segments.next()?, segments.next()?, segments.next()?);
        let rest = segments.next().unwrap_or("");
        let reference = match kind {
            "tree" => Some(rest.strip_suffix('/').unwrap_or(rest)),
            "archive" => [".zip", ".tar.gz"]
                .iter()
                .find_map(|ext| rest.strip_suffix(ext)),
            _ => return None,
        };
        Some(reference.and_then(|reference| Self::pinned(owner, repo, reference)))
    }

    /// `owner/repo` at `reference`, all validated.
    fn pinned(owner: &str, repo: &str, reference: &str) -> Option<Self> {
        if !valid_reference(reference) || reference.contains('#') {
            return None;
        }
        let (owner, repo) =
            github_owner_repo(&format!("https://github.com/{owner}/{repo}")).ok()?;
        Some(Self {
            owner,
            repo,
            reference: Some(reference.to_string()),
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
