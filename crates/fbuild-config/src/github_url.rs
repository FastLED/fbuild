//! GitHub URL shapes that name one ref (FastLED/fbuild#2042, #2125).
//!
//! GitHub's web URL of a ref (`…/<owner>/<repo>/tree/<ref>`) and its direct
//! archive URLs (`…/archive/<ref>.zip`, `…/archive/refs/tags/<tag>.tar.gz`)
//! each name one ref, like the `#<ref>` suffix on a repository URL.
//!
//! Build installs (`fbuild_library::library::GitHubSource`) and `fbuild sync`
//! / `/api/ide/libraries` ([`crate::lib_source::classify`]) both read these
//! shapes, so the parser lives here, below both. It is pure string work with
//! no dependencies; owner/repo validation against GitHub's naming rules stays
//! with the installer.

/// A GitHub web or archive URL's repository and ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubWebRef<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    /// The ref, percent-decoded once and validated with [`valid_reference`].
    pub reference: String,
}

/// `http://` and `www.` spellings of github.com name the same repository;
/// spell them as the canonical `https://github.com/` that [`parse_web_url`]
/// expects. A `git+` prefix is kept.
pub fn canonical_github_host(spec: &str) -> String {
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

/// GitHub's web URL of a ref, `https://github.com/<o>/<r>/tree/<ref>`, or a
/// direct archive URL, `…/<o>/<r>/archive/<ref>.zip|.tar.gz` (including the
/// `refs/tags/<t>` and `refs/heads/<b>` forms).
///
/// `spec` must already be canonical ([`canonical_github_host`]). `None` when
/// the URL has neither shape; `Some(None)` when it has one but the ref is
/// missing or malformed.
pub fn parse_web_url(spec: &str) -> Option<Option<GitHubWebRef<'_>>> {
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
    // The ref is a URL path here, so it may be percent-encoded
    // (`feature%2Fx`); decode it once (FastLED/fbuild#2042).
    let reference = reference
        .and_then(percent_decode)
        .filter(|reference| valid_reference(reference) && !reference.contains('#'));
    Some(reference.map(|reference| GitHubWebRef {
        owner,
        repo,
        reference,
    }))
}

/// A ref must be a usable URL path: no whitespace or control characters, no
/// `?`, and no empty, `.` or `..` path segments.
pub fn valid_reference(reference: &str) -> bool {
    !reference.is_empty()
        && !reference
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '?')
        && reference
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
}

/// Decode `%XX` escapes in a URL path. `None` for a malformed escape or a
/// result that is not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = tail
                .get(..2)
                .filter(|h| h.iter().all(u8::is_ascii_hexdigit))?;
            let hex = std::str::from_utf8(hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(spec: &str) -> Option<Option<String>> {
        parse_web_url(spec).map(|web| web.map(|web| web.reference))
    }

    #[test]
    fn tree_and_archive_urls_name_their_ref() {
        for (spec, expected) in [
            ("https://github.com/O/R/tree/v1", "v1"),
            ("https://github.com/O/R/tree/v1/", "v1"),
            ("https://github.com/O/R/tree/feature/x", "feature/x"),
            ("https://github.com/O/R/archive/v1.zip", "v1"),
            (
                "https://github.com/O/R/archive/refs/tags/v1.zip",
                "refs/tags/v1",
            ),
            (
                "https://github.com/O/R/archive/refs/heads/main.tar.gz",
                "refs/heads/main",
            ),
        ] {
            let web = parse_web_url(spec)
                .flatten()
                .unwrap_or_else(|| panic!("{spec}"));
            assert_eq!((web.owner, web.repo), ("O", "R"), "{spec}");
            assert_eq!(web.reference, expected, "{spec}");
        }
    }

    #[test]
    fn percent_encoded_ref_is_decoded_once() {
        for spec in [
            "https://github.com/O/R/tree/feature%2Fx",
            "https://github.com/O/R/tree/feature%2fx",
            "https://github.com/O/R/archive/feature%2Fx.zip",
        ] {
            assert_eq!(reference(spec), Some(Some("feature/x".into())), "{spec}");
        }
        assert_eq!(
            reference("https://github.com/O/R/tree/v1%2B2"),
            Some(Some("v1+2".into()))
        );
    }

    #[test]
    fn malformed_ref_is_some_none() {
        for spec in [
            "https://github.com/O/R/tree",
            "https://github.com/O/R/tree/",
            "https://github.com/O/R/tree/a/../b",
            "https://github.com/O/R/tree/a%2F..%2Fb",
            "https://github.com/O/R/tree/v%201",
            "https://github.com/O/R/tree/v%zz",
            "https://github.com/O/R/tree/v%FF",
            "https://github.com/O/R/tree/v1?x=1",
            "https://github.com/O/R/archive/v1",
            "https://github.com/O/R/archive/.zip",
            "https://github.com/O/R/archive/v1.zip#v2",
        ] {
            assert_eq!(reference(spec), Some(None), "{spec}");
        }
    }

    #[test]
    fn other_shapes_are_none() {
        for spec in [
            "https://github.com/O/R",
            "https://github.com/O/R.git",
            "https://github.com/O/R/blob/main/README.md",
            "https://github.com/O/R/releases/download/v1/R.zip",
            "git+https://github.com/O/R/tree/v1",
            "http://github.com/O/R/tree/v1",
            "https://gitlab.com/O/R/tree/v1",
        ] {
            assert_eq!(reference(spec), None, "{spec}");
        }
    }

    #[test]
    fn canonical_host_rewrites_http_and_www_only() {
        for (spec, expected) in [
            ("http://github.com/O/R", "https://github.com/O/R"),
            (
                "http://www.github.com/O/R/tree/v1",
                "https://github.com/O/R/tree/v1",
            ),
            ("https://www.github.com/O/R", "https://github.com/O/R"),
            (
                "git+http://github.com/O/R.git",
                "git+https://github.com/O/R.git",
            ),
            ("https://github.com/O/R", "https://github.com/O/R"),
            ("https://gitlab.com/O/R", "https://gitlab.com/O/R"),
        ] {
            assert_eq!(canonical_github_host(spec), expected, "{spec}");
        }
    }

    #[test]
    fn reference_validation() {
        assert!(valid_reference("v1.2.3"));
        assert!(valid_reference("feature/x"));
        for bad in ["", "a b", "a?b", "a//b", "./a", "a/..", "a\nb"] {
            assert!(!valid_reference(bad), "{bad:?}");
        }
    }
}
