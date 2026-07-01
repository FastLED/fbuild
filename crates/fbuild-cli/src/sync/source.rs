//! Classification of PlatformIO `lib_deps` entries into source types.
//!
//! FastLED/fbuild#618 Phase 1. Reads a raw string as parsed by
//! `fbuild_config::ini_parser::parse_lib_deps` and figures out what kind
//! of dependency source it is (registry, GitHub, generic Git, HTTP archive,
//! symlink, `file://`, or local path). Local sources map to
//! [`LockStatus::Unlocked`] in the lockfile — they can be represented but
//! not reproducibly locked. Remote sources map to
//! [`LockStatus::Unresolved`] pending Phase 2 network resolution.
//!
//! PlatformIO's `lib_deps` grammar is documented at
//! <https://docs.platformio.org/en/latest/projectconf/sections/env/options/library/lib_deps.html>.

use serde::{Deserialize, Serialize};

/// Kind of source declared in a `lib_deps` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceType {
    /// PlatformIO Library Registry — `<name>`, `<name>@<version>`, or
    /// `<owner>/<name>@<version>`. Reproducible: version resolves to a
    /// registry version identifier + archive URL + sha256 during Phase 2
    /// resolution.
    Registry,
    /// GitHub URL — `https://github.com/<owner>/<repo>[.git][#<ref>]`.
    /// `<ref>` may be a branch, tag, or commit SHA per PlatformIO docs.
    /// Reproducible via the resolved SHA.
    Github,
    /// Generic Git URL (non-GitHub) with an explicit `git+` prefix.
    /// Reproducible via commit SHA.
    Git,
    /// HTTP or HTTPS archive URL (`http[s]://...zip|tar.gz|tar.bz2`).
    /// Reproducible via sha256 of the fetched archive.
    HttpArchive,
    /// PlatformIO's `symlink://<path>` scheme. Local reference, not
    /// reproducible off the developer's machine.
    Symlink,
    /// `file://<path>` scheme. Local, not reproducible.
    File,
    /// Bare filesystem path — relative (`./libs/foo`, `../foo`) or
    /// absolute (`/home/foo`, `C:\libs\foo`). Local, not reproducible.
    LocalPath,
}

impl SourceType {
    /// True when the entry can never be reproducibly locked from off-machine
    /// tooling (developer symlinks, `file://` URIs, filesystem paths). The
    /// lockfile records the raw spec but marks such entries as
    /// [`LockStatus::Unlocked`].
    #[must_use]
    pub fn is_local(self) -> bool {
        matches!(
            self,
            SourceType::Symlink | SourceType::File | SourceType::LocalPath
        )
    }
}

/// Status of a package inside the lockfile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LockStatus {
    /// Locked to a resolved identity (commit SHA + archive sha256).
    /// Not produced by Phase 1 — reserved for Phase 2 network resolution.
    Locked,
    /// Remote source that Phase 1 has parsed but not yet resolved (waiting
    /// on Phase 2 network wiring). The raw spec is captured so future
    /// `fbuild sync --upgrade` can pick it up.
    Unresolved,
    /// Local source (symlink / `file://` / path) — not reproducibly lockable.
    /// The lockfile records it for auditing but marks it explicitly.
    Unlocked,
}

/// A `lib_deps` entry after classification. `raw` is the original string;
/// `name` is a best-effort human-facing label; the remaining fields are
/// source-type-specific pieces we can extract without a network round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassifiedDep {
    /// The exact `lib_deps` string as parsed from `platformio.ini`.
    pub raw: String,
    /// Human-facing label. For registry entries it's the package name
    /// (`FastLED`); for VCS it's the repo name (`FastLED`); for local
    /// paths it's the trailing directory (`mylib`). Never empty.
    pub name: String,
    /// Which class of source this is.
    pub source_type: SourceType,
    /// Registry / VCS version constraint (`^3.5.0`, `~1.0`, `v2.0.0`) or
    /// VCS ref (`main`, `v1.2.3`) — whatever appears after `@` or `#` on
    /// the raw spec. `None` if none was provided.
    pub version_spec: Option<String>,
    /// Owner segment for `owner/name@ver` registry specs, or for
    /// `github.com/<owner>/<repo>` URLs.
    pub owner: Option<String>,
    /// The exact URL for `github.com/...`, `git+...`, `http[s]://...zip`,
    /// `symlink://...`, or `file://...` entries. `None` for registry and
    /// bare-path entries.
    pub url: Option<String>,
    /// Resolved absolute-ish path for `symlink://`, `file://`, or bare-path
    /// entries. The path may not exist yet on disk — we don't stat it here.
    pub local_path: Option<String>,
}

impl ClassifiedDep {
    /// The lockfile status this entry should carry when written by
    /// Phase 1 (no network resolution yet).
    #[must_use]
    pub fn phase1_lock_status(&self) -> LockStatus {
        if self.source_type.is_local() {
            LockStatus::Unlocked
        } else {
            LockStatus::Unresolved
        }
    }
}

/// Classify one `lib_deps` entry.
///
/// The input has already been through `fbuild_config::ini_parser::
/// parse_lib_deps`, so it's trimmed of surrounding whitespace and
/// comments. This function does zero I/O.
#[must_use]
pub fn classify(raw: &str) -> ClassifiedDep {
    let trimmed = raw.trim();

    // 1. Explicit `symlink://` — takes priority over path detection.
    if let Some(rest) = trimmed.strip_prefix("symlink://") {
        return ClassifiedDep {
            raw: raw.to_string(),
            name: last_segment(rest).to_string(),
            source_type: SourceType::Symlink,
            version_spec: None,
            owner: None,
            url: Some(trimmed.to_string()),
            local_path: Some(rest.to_string()),
        };
    }

    // 2. `file://` — RFC 3986 file scheme.
    if let Some(rest) = trimmed.strip_prefix("file://") {
        return ClassifiedDep {
            raw: raw.to_string(),
            name: last_segment(rest).to_string(),
            source_type: SourceType::File,
            version_spec: None,
            owner: None,
            url: Some(trimmed.to_string()),
            local_path: Some(rest.to_string()),
        };
    }

    // 3. `git+<url>` — explicit generic-Git prefix.
    if let Some(rest) = trimmed.strip_prefix("git+") {
        let (url, git_ref) = split_ref(rest);
        return ClassifiedDep {
            raw: raw.to_string(),
            name: repo_name_from_git_url(&url).unwrap_or_else(|| last_segment(&url).to_string()),
            source_type: SourceType::Git,
            version_spec: git_ref,
            owner: None,
            url: Some(url),
            local_path: None,
        };
    }

    // 4. HTTP/HTTPS — could be GitHub or a plain archive. Case-insensitive
    //    on the scheme because PlatformIO's `lib_deps` treats URLs as
    //    URIs per RFC 3986 §3.1 (scheme is case-insensitive), and the
    //    `github_case_insensitive_host` test expects `HTTPS://GITHUB.COM/...`
    //    to still classify as Github.
    if starts_with_ci(trimmed, "http://") || starts_with_ci(trimmed, "https://") {
        // Split off the optional `#<ref>` (only meaningful for repo URLs;
        // if it's on an archive URL it's harmless noise).
        let (url_no_ref, hash_suffix) = split_ref(trimmed);
        // GitHub? Match both `github.com/owner/repo` and
        // `github.com/owner/repo.git`.
        if is_github_url(&url_no_ref) {
            let (owner, repo) = github_owner_repo(&url_no_ref).unwrap_or_default();
            return ClassifiedDep {
                raw: raw.to_string(),
                name: repo.clone(),
                source_type: SourceType::Github,
                version_spec: hash_suffix,
                owner: if owner.is_empty() { None } else { Some(owner) },
                url: Some(url_no_ref),
                local_path: None,
            };
        }
        // Non-GitHub archive URLs.
        return ClassifiedDep {
            raw: raw.to_string(),
            name: archive_name_from_url(&url_no_ref).unwrap_or_else(|| {
                last_segment(&url_no_ref).trim_end_matches(".git").to_string()
            }),
            source_type: SourceType::HttpArchive,
            version_spec: hash_suffix,
            owner: None,
            url: Some(url_no_ref),
            local_path: None,
        };
    }

    // 5. Filesystem path — absolute (`/`, `C:\`, `C:/`) or relative
    //    (`./`, `../`). We check this BEFORE registry so a Unix
    //    absolute path like `/home/foo/lib` isn't treated as a
    //    registry name.
    if is_local_path(trimmed) {
        return ClassifiedDep {
            raw: raw.to_string(),
            name: last_segment(trimmed).to_string(),
            source_type: SourceType::LocalPath,
            version_spec: None,
            owner: None,
            url: None,
            local_path: Some(trimmed.to_string()),
        };
    }

    // 6. Registry — bare name, name@ver, or owner/name@ver.
    let (owner, name, version) = split_registry_spec(trimmed);
    ClassifiedDep {
        raw: raw.to_string(),
        name,
        source_type: SourceType::Registry,
        version_spec: version,
        owner,
        url: None,
        local_path: None,
    }
}

// ---------- helpers ----------

/// ASCII case-insensitive `starts_with`, used for the RFC 3986 URI
/// scheme prefix check in `classify()`.
fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// Take the last `/` or `\` segment of a string, trimming any trailing
/// `.git`. Never returns empty — falls back to the input.
fn last_segment(s: &str) -> &str {
    let end = s.trim_end_matches(['/', '\\']);
    let seg = end.rsplit(['/', '\\']).next().unwrap_or(end);
    let seg = seg.trim_end_matches(".git");
    if seg.is_empty() {
        end
    } else {
        seg
    }
}

/// Split `url#ref` into `(url, Some(ref))`. Returns `(input, None)` when
/// there's no `#`. `#` before the last path segment is treated as part of
/// the URL (PlatformIO's convention is `<url>#<ref>` at the very end).
fn split_ref(s: &str) -> (String, Option<String>) {
    if let Some((left, right)) = s.rsplit_once('#') {
        (left.to_string(), Some(right.to_string()))
    } else {
        (s.to_string(), None)
    }
}

fn is_github_url(url: &str) -> bool {
    // Match http[s]://github.com/... and http[s]://www.github.com/...
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://github.com/")
        || lower.starts_with("http://github.com/")
        || lower.starts_with("https://www.github.com/")
        || lower.starts_with("http://www.github.com/")
}

/// Extract `(owner, repo)` from `https://github.com/<owner>/<repo>[.git]`.
fn github_owner_repo(url: &str) -> Option<(String, String)> {
    let after_host = url
        .split_once("github.com/")
        .map(|(_, rest)| rest)?;
    let mut parts = after_host.splitn(3, '/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Extract a human-facing name for a Git URL when it's not GitHub. Looks
/// at the trailing path segment stripped of `.git`.
fn repo_name_from_git_url(url: &str) -> Option<String> {
    let stripped = url.trim_end_matches(".git");
    let last = last_segment(stripped);
    if last.is_empty() || last == stripped {
        return None;
    }
    Some(last.to_string())
}

/// Try to extract a `-a.b.c` style archive name from a URL like
/// `.../FastLED-3.5.0.zip` → `FastLED`.
fn archive_name_from_url(url: &str) -> Option<String> {
    let last = last_segment(url);
    // Strip common extensions.
    let stem = last
        .strip_suffix(".tar.gz")
        .or_else(|| last.strip_suffix(".tar.bz2"))
        .or_else(|| last.strip_suffix(".tar.xz"))
        .or_else(|| last.strip_suffix(".tgz"))
        .or_else(|| last.strip_suffix(".tbz2"))
        .or_else(|| last.strip_suffix(".zip"))
        .unwrap_or(last);
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_string())
}

fn is_local_path(s: &str) -> bool {
    if s.starts_with("./") || s.starts_with("../") || s == "." || s == ".." {
        return true;
    }
    if s.starts_with('/') || s.starts_with('\\') {
        return true;
    }
    // Windows drive letter: `C:\...`, `C:/...`, `D:\...`, etc.
    let bytes = s.as_bytes();
    if bytes.len() >= 3
        && (bytes[0].is_ascii_alphabetic())
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    false
}

/// Split a registry spec `[owner/]name[@version]` into its parts.
fn split_registry_spec(s: &str) -> (Option<String>, String, Option<String>) {
    // `@` splits `name-or-slash-part` from version.
    let (left, version) = match s.split_once('@') {
        Some((l, r)) => (l, Some(r.to_string())),
        None => (s, None),
    };
    // `/` splits owner from name.
    let (owner, name) = match left.split_once('/') {
        Some((o, n)) if !o.is_empty() && !n.is_empty() => (Some(o.to_string()), n.to_string()),
        _ => (None, left.to_string()),
    };
    (owner, name, version)
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn cls(s: &str) -> ClassifiedDep {
        classify(s)
    }

    // Registry
    #[test]
    fn registry_bare_name() {
        let d = cls("FastLED");
        assert_eq!(d.source_type, SourceType::Registry);
        assert_eq!(d.name, "FastLED");
        assert_eq!(d.owner, None);
        assert_eq!(d.version_spec, None);
    }

    #[test]
    fn registry_name_with_version() {
        let d = cls("FastLED@^3.5.0");
        assert_eq!(d.source_type, SourceType::Registry);
        assert_eq!(d.name, "FastLED");
        assert_eq!(d.version_spec.as_deref(), Some("^3.5.0"));
        assert_eq!(d.owner, None);
    }

    #[test]
    fn registry_owner_slash_name_with_version() {
        let d = cls("fastled/FastLED@^3.5.0");
        assert_eq!(d.source_type, SourceType::Registry);
        assert_eq!(d.name, "FastLED");
        assert_eq!(d.owner.as_deref(), Some("fastled"));
        assert_eq!(d.version_spec.as_deref(), Some("^3.5.0"));
    }

    #[test]
    fn registry_status_is_unresolved_in_phase1() {
        assert_eq!(cls("FastLED").phase1_lock_status(), LockStatus::Unresolved);
    }

    // GitHub
    #[test]
    fn github_https_bare() {
        let d = cls("https://github.com/FastLED/FastLED");
        assert_eq!(d.source_type, SourceType::Github);
        assert_eq!(d.name, "FastLED");
        assert_eq!(d.owner.as_deref(), Some("FastLED"));
        assert_eq!(d.url.as_deref(), Some("https://github.com/FastLED/FastLED"));
        assert_eq!(d.version_spec, None);
    }

    #[test]
    fn github_https_with_dotgit() {
        let d = cls("https://github.com/FastLED/FastLED.git");
        assert_eq!(d.source_type, SourceType::Github);
        assert_eq!(d.name, "FastLED");
        assert_eq!(d.owner.as_deref(), Some("FastLED"));
    }

    #[test]
    fn github_https_with_hash_ref() {
        let d = cls("https://github.com/FastLED/FastLED#v3.5.0");
        assert_eq!(d.source_type, SourceType::Github);
        assert_eq!(d.name, "FastLED");
        assert_eq!(d.version_spec.as_deref(), Some("v3.5.0"));
        assert_eq!(d.url.as_deref(), Some("https://github.com/FastLED/FastLED"));
    }

    #[test]
    fn github_case_insensitive_host() {
        let d = cls("HTTPS://GITHUB.COM/FastLED/FastLED");
        assert_eq!(d.source_type, SourceType::Github);
    }

    // Generic Git
    #[test]
    fn git_prefix_url() {
        let d = cls("git+https://gitlab.com/foo/bar.git");
        assert_eq!(d.source_type, SourceType::Git);
        assert_eq!(d.name, "bar");
        assert_eq!(d.url.as_deref(), Some("https://gitlab.com/foo/bar.git"));
    }

    #[test]
    fn git_prefix_with_ref() {
        let d = cls("git+https://gitlab.com/foo/bar.git#v1.0");
        assert_eq!(d.source_type, SourceType::Git);
        assert_eq!(d.version_spec.as_deref(), Some("v1.0"));
    }

    // HTTP archive
    #[test]
    fn http_archive_zip() {
        let d = cls("https://example.com/downloads/FastLED-3.5.0.zip");
        assert_eq!(d.source_type, SourceType::HttpArchive);
        assert_eq!(d.name, "FastLED-3.5.0");
        assert!(d.url.as_deref().unwrap().ends_with(".zip"));
    }

    #[test]
    fn http_archive_tarball() {
        let d = cls("https://example.com/downloads/mylib.tar.gz");
        assert_eq!(d.source_type, SourceType::HttpArchive);
        assert_eq!(d.name, "mylib");
    }

    // Local sources
    #[test]
    fn symlink_uri() {
        let d = cls("symlink:///home/foo/lib");
        assert_eq!(d.source_type, SourceType::Symlink);
        assert_eq!(d.name, "lib");
        assert_eq!(d.local_path.as_deref(), Some("/home/foo/lib"));
        assert_eq!(d.phase1_lock_status(), LockStatus::Unlocked);
    }

    #[test]
    fn file_uri() {
        let d = cls("file:///home/foo/lib");
        assert_eq!(d.source_type, SourceType::File);
        assert_eq!(d.name, "lib");
        assert_eq!(d.phase1_lock_status(), LockStatus::Unlocked);
    }

    #[test]
    fn relative_path() {
        let d = cls("./libs/mylib");
        assert_eq!(d.source_type, SourceType::LocalPath);
        assert_eq!(d.name, "mylib");
        assert_eq!(d.local_path.as_deref(), Some("./libs/mylib"));
        assert_eq!(d.phase1_lock_status(), LockStatus::Unlocked);
    }

    #[test]
    fn relative_dotdot_path() {
        let d = cls("../shared/lib");
        assert_eq!(d.source_type, SourceType::LocalPath);
        assert_eq!(d.name, "lib");
    }

    #[test]
    fn absolute_posix_path() {
        let d = cls("/home/foo/lib");
        assert_eq!(d.source_type, SourceType::LocalPath);
        assert_eq!(d.name, "lib");
    }

    #[test]
    fn absolute_windows_path_backslash() {
        let d = cls(r"C:\libs\mylib");
        assert_eq!(d.source_type, SourceType::LocalPath);
        assert_eq!(d.name, "mylib");
    }

    #[test]
    fn absolute_windows_path_forward_slash() {
        let d = cls("D:/libs/mylib");
        assert_eq!(d.source_type, SourceType::LocalPath);
        assert_eq!(d.name, "mylib");
    }

    // Whitespace tolerance
    #[test]
    fn whitespace_is_trimmed() {
        let d = cls("  FastLED@^3.5.0  ");
        assert_eq!(d.source_type, SourceType::Registry);
        assert_eq!(d.name, "FastLED");
    }

    // Round-trip of `raw`
    #[test]
    fn raw_is_preserved_verbatim() {
        // Even if we trim during parsing, the original raw string is
        // kept in the record so an operator can eyeball the lockfile
        // and see exactly what was in platformio.ini.
        let d = cls("  ./libs/mylib  ");
        assert_eq!(d.raw, "  ./libs/mylib  ");
    }
}
