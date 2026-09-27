//! Detect a core unpacked without its git submodule contents.
//!
//! GitHub's auto-generated source archives (`archive/refs/tags/…`) omit
//! submodules by design: the directories are created, the contents are not.
//! Several Arduino cores keep libraries as submodules, so an archive-sourced
//! package extracts to something that looks complete and fails much later,
//! inside the core's own headers.
//!
//! FastLED/fbuild#1380 is the worked example. `esp8266/Arduino` carries
//! `libraries/LittleFS/lib/littlefs`, so `#include <LittleFS.h>` reached
//!
//! ```text
//! LittleFS.h:38:10: fatal error: ../lib/littlefs/lfs.h: No such file
//! ```
//!
//! and `__has_include(<LittleFS.h>)` still passed, because the header was
//! present and only the thing it includes was absent. No consumer-side guard
//! can detect that.
//!
//! The archive carries `.gitmodules` even when it drops the submodule
//! contents, which is what makes this cheap to catch: the file states exactly
//! which directories are supposed to be non-empty.
//!
//! Not every core can switch to an archive that bundles its submodules. A
//! [`SubmodulePlan`] covers those: a pinned source fills a submodule during
//! install (FastLED/fbuild#1420), and an expected-empty entry records one whose
//! contents fbuild supplies another way (FastLED/fbuild#1421, #1422). Both are
//! checked against `.gitmodules`, so a plan that no longer matches upstream
//! fails instead of rotting.

use std::path::Path;

use fbuild_core::path::NormalizedPath;

use crate::PackageBase;

/// A declared submodule whose directory came out of the archive empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptySubmodule {
    /// Path as written in `.gitmodules`, relative to the repo root.
    pub declared_path: String,
    /// Where that landed on disk.
    pub extracted_at: NormalizedPath,
}

/// Parse the `path = …` entries out of a `.gitmodules` file.
///
/// Deliberately not a full INI parse. `.gitmodules` is written by git, the
/// only field this needs is `path`, and a permissive line scan cannot fail
/// closed on an unusual-but-valid file the way a strict parser can.
pub fn declared_submodule_paths(gitmodules: &str) -> Vec<String> {
    gitmodules
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            if key.trim() != "path" {
                return None;
            }
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        })
        .collect()
}

/// Whether a directory has no entries. A missing directory is *not* empty for
/// this purpose: git records the submodule directory itself in the archive, so
/// its absence means something else went wrong and this check should not
/// claim otherwise.
fn is_empty_dir(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => false,
    }
}

/// Report every declared submodule that extracted empty under `root`.
///
/// Returns an empty vec when `root` has no `.gitmodules` — most packages are
/// not git repositories at all, and their absence is the normal case rather
/// than a problem.
pub fn find_empty_submodules(root: &Path) -> Vec<EmptySubmodule> {
    let gitmodules = root.join(".gitmodules");
    let Ok(text) = std::fs::read_to_string(&gitmodules) else {
        return Vec::new();
    };

    declared_submodule_paths(&text)
        .into_iter()
        .filter_map(|declared| {
            let extracted_at = root.join(&declared);
            is_empty_dir(&extracted_at).then(|| EmptySubmodule {
                declared_path: declared,
                extracted_at: NormalizedPath::from(extracted_at),
            })
        })
        .collect()
}

/// Message for a package that unpacked without its submodule contents.
///
/// Names the empty directories and the likely cause, because the symptom this
/// prevents — a missing header several layers inside a core — gives the reader
/// nothing to work with.
pub fn empty_submodule_error(package: &str, url: &str, empty: &[EmptySubmodule]) -> String {
    let listed = empty
        .iter()
        .map(|e| format!("  - {}", e.declared_path))
        .collect::<Vec<_>>()
        .join("\n");
    let source_archive_hint = if url.contains("/archive/refs/") {
        "\n\nThe URL above is a GitHub auto-generated source archive, which \
         omits submodules by design. Use the release asset published on the \
         tag if the project provides one (that is what FastLED/fbuild#1380 \
         did for esp8266), or fetch with submodules."
    } else {
        "\n\nThe archive declares these submodules but shipped them empty."
    };
    format!(
        "{package} unpacked without its submodule contents. These directories \
         are declared in .gitmodules and came out empty:\n{listed}\n\nurl: \
         {url}{source_archive_hint}\n\nLeaving this to the compiler produces a \
         missing-header error inside the core, past any `__has_include` guard \
         a sketch could write (FastLED/fbuild#1380)."
    )
}

/// Contents for a declared submodule, fetched from a pinned archive.
///
/// For a core whose upstream publishes no archive that bundles its
/// submodules, so the URL swap FastLED/fbuild#1380 made for esp8266 is not
/// available. `ch32v-core` is the case: openwch ships no release assets, and
/// the pinned commit postdates every release (FastLED/fbuild#1420).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmoduleSource {
    /// Path as written in `.gitmodules`, relative to the repo root.
    pub path: String,
    /// Archive of the submodule at the commit the parent's gitlink records.
    pub url: String,
    /// SHA-256 of that archive.
    pub sha256: Option<String>,
}

/// A declared submodule left empty on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedEmpty {
    /// Path as written in `.gitmodules`, relative to the repo root.
    pub path: String,
    /// How fbuild supplies the contents instead.
    pub reason: String,
}

/// How a package's declared submodules are handled at unpack. Empty for
/// almost every package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubmodulePlan {
    pub sources: Vec<SubmoduleSource>,
    pub expected_empty: Vec<ExpectedEmpty>,
    pub github_gitlinks: bool,
}

impl SubmodulePlan {
    fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.expected_empty.is_empty() && !self.github_gitlinks
    }
}

impl PackageBase {
    /// Fill a declared submodule from a pinned archive during install, for a
    /// core whose upstream publishes no archive that bundles its submodules
    /// (FastLED/fbuild#1420).
    pub fn with_submodule_source(mut self, path: &str, url: &str, sha256: &str) -> Self {
        self.submodules.sources.push(SubmoduleSource {
            path: path.to_string(),
            url: url.to_string(),
            sha256: Some(sha256.to_string()),
        });
        self
    }

    /// Resolve empty submodules from the selected GitHub archive's immutable
    /// parent gitlinks, instead of reusing pins from another core revision.
    pub fn with_github_gitlinks(mut self) -> Self {
        self.submodules.github_gitlinks = true;
        self
    }

    /// Accept a declared submodule that extracts empty because fbuild supplies
    /// its contents another way. `reason` says how (FastLED/fbuild#1422).
    pub fn expect_empty_submodule(mut self, path: &str, reason: &str) -> Self {
        self.submodules.expected_empty.push(ExpectedEmpty {
            path: path.to_string(),
            reason: reason.to_string(),
        });
        self
    }
}

/// Apply `plan` to a freshly extracted package, then reject any declared
/// submodule that is still empty.
///
/// Checked against the extracted root and one level down, since most
/// archives nest under a single version directory (`esp8266-3.1.2/`).
pub async fn prepare_submodules(
    package: &str,
    url: &str,
    staging: &Path,
    plan: &SubmodulePlan,
) -> fbuild_core::Result<()> {
    let children = std::fs::read_dir(staging)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir());
    let roots: Vec<_> = std::iter::once(staging.to_path_buf())
        .chain(children)
        .collect();

    let mut applied = false;
    for root in &roots {
        let Ok(text) = std::fs::read_to_string(root.join(".gitmodules")) else {
            continue;
        };
        let declared = declared_submodule_paths(&text);
        let mut resolved_plan = plan.clone();
        if plan.github_gitlinks {
            resolved_plan.sources.extend(
                github_submodule_sources(url, root, &text, &declared)
                    .await
                    .map_err(|error| {
                        fbuild_core::FbuildError::PackageError(format!(
                            "{package}: resolving GitHub submodules failed: {error}"
                        ))
                    })?,
            );
        }
        check_plan(package, root, &declared, &resolved_plan)?;
        populate(package, root, &resolved_plan.sources).await?;
        applied = true;

        let empty = unexpected_empty_submodules(root, &resolved_plan);
        if !empty.is_empty() {
            return Err(fbuild_core::FbuildError::PackageError(
                empty_submodule_error(package, url, &empty),
            ));
        }
        for entry in &plan.expected_empty {
            tracing::debug!(
                "{package}: submodule {} left empty: {}",
                entry.path,
                entry.reason
            );
        }
    }

    if !plan.is_empty() && !applied {
        return Err(fbuild_core::FbuildError::PackageError(format!(
            "{package} has a submodule plan, but its archive has no .gitmodules. \
             The plan is stale and should be removed."
        )));
    }
    Ok(())
}

async fn github_submodule_sources(
    parent_archive: &str,
    root: &Path,
    gitmodules: &str,
    declared: &[String],
) -> fbuild_core::Result<Vec<SubmoduleSource>> {
    let (owner, repo, commit) = github_archive_identity(parent_archive)?;
    let checkout = tempfile::TempDir::new().map_err(package_error)?;
    let git_dir = checkout
        .path()
        .to_str()
        .ok_or_else(|| package_error("invalid temporary Git path"))?;
    git_command(["init", "--bare", "--quiet", git_dir]).await?;
    let repository = format!("https://github.com/{owner}/{repo}.git");
    git_command([
        "-C",
        git_dir,
        "fetch",
        "--quiet",
        "--depth",
        "1",
        "--filter=blob:none",
        "--no-tags",
        &repository,
        &commit,
    ])
    .await?;
    let declarations = declared_submodule_urls(gitmodules);
    let mut sources = Vec::new();
    for path in declared {
        if !is_empty_dir(&root.join(path)) {
            continue;
        }
        checked_submodule_dest(root, path)?;
        let source_url = declarations
            .iter()
            .find(|(declared_path, _)| declared_path == path)
            .map(|(_, url)| url)
            .ok_or_else(|| {
                package_error(format!("submodule `{path}` has no URL in .gitmodules"))
            })?;
        let listing = git_command(["-C", git_dir, "ls-tree", &commit, "--", path]).await?;
        let mut words = listing.split_whitespace();
        if words.next() != Some("160000") || words.next() != Some("commit") {
            return Err(package_error(format!("submodule `{path}` has no gitlink")));
        }
        let gitlink = words
            .next()
            .ok_or_else(|| package_error("missing gitlink SHA"))?;
        if !full_sha(gitlink) {
            return Err(package_error(format!(
                "submodule `{path}` has invalid gitlink"
            )));
        }
        let (sub_owner, sub_repo) = github_repository_identity(source_url)?;
        sources.push(SubmoduleSource {
            path: path.clone(),
            url: format!("https://github.com/{sub_owner}/{sub_repo}/archive/{gitlink}.tar.gz"),
            // The immutable gitlink is the source identity. GitHub does not
            // publish a SHA-256 for its generated archive.
            sha256: None,
        });
    }
    Ok(sources)
}

fn checked_submodule_dest(root: &Path, path: &str) -> fbuild_core::Result<std::path::PathBuf> {
    if path.starts_with('/')
        || path
            .split('/')
            .any(|component| matches!(component, "" | "." | ".."))
        || path.contains(['\\', '\n', '\r'])
    {
        return Err(package_error(format!("unsafe submodule path `{path}`")));
    }
    let canonical_root = root.canonicalize().map_err(package_error)?;
    let canonical_dest = root.join(path).canonicalize().map_err(package_error)?;
    if !canonical_dest.starts_with(&canonical_root) {
        return Err(package_error(format!(
            "submodule `{path}` escapes the extracted package"
        )));
    }
    Ok(canonical_dest)
}

async fn git_command<const N: usize>(args: [&str; N]) -> fbuild_core::Result<String> {
    // allow-direct-spawn: Git resolves immutable VCS gitlinks without a shell
    // or GitHub REST API quota, matching PlatformIO's VCS dependency.
    let mut command = tokio::process::Command::new("git");
    command.args(args).kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), command.output())
        .await
        .map_err(|_| package_error("timed out resolving GitHub submodule gitlink"))?
        .map_err(package_error)?;
    if !output.status.success() {
        return Err(package_error(format!(
            "git exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout).map_err(package_error)
}

fn github_archive_identity(url: &str) -> fbuild_core::Result<(String, String, String)> {
    let parsed = reqwest::Url::parse(url).map_err(package_error)?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(package_error(format!("not a GitHub archive: {url}")));
    }
    let parts: Vec<_> = parsed
        .path_segments()
        .ok_or_else(|| package_error("GitHub archive has no path"))?
        .collect();
    let [owner, repo, "archive", file] = parts.as_slice() else {
        return Err(package_error(format!(
            "unsupported GitHub archive URL: {url}"
        )));
    };
    let commit = file
        .strip_suffix(".tar.gz")
        .ok_or_else(|| package_error("GitHub archive is not a tar.gz"))?;
    if !valid_github_segment(owner) || !valid_github_segment(repo) || !full_sha(commit) {
        return Err(package_error(format!(
            "invalid GitHub archive identity: {url}"
        )));
    }
    Ok(((*owner).into(), (*repo).into(), commit.into()))
}

/// Whether a package URL names an immutable GitHub commit archive whose
/// submodule gitlinks can be resolved from the same parent commit.
pub fn is_github_commit_archive_url(url: &str) -> bool {
    github_archive_identity(url).is_ok()
}

fn github_repository_identity(url: &str) -> fbuild_core::Result<(String, String)> {
    let parsed = reqwest::Url::parse(url).map_err(package_error)?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("github.com") {
        return Err(package_error(format!(
            "unsupported submodule repository: {url}"
        )));
    }
    let parts: Vec<_> = parsed
        .path_segments()
        .ok_or_else(|| package_error("submodule repository has no path"))?
        .collect();
    let [owner, repo] = parts.as_slice() else {
        return Err(package_error(format!(
            "invalid submodule repository: {url}"
        )));
    };
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if !valid_github_segment(owner) || !valid_github_segment(repo) {
        return Err(package_error(format!(
            "invalid submodule repository: {url}"
        )));
    }
    Ok(((*owner).into(), repo.into()))
}

fn valid_github_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn declared_submodule_urls(gitmodules: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut path = None;
    let mut url = None;
    for line in gitmodules.lines().chain(std::iter::once("[end]")) {
        let line = line.trim();
        if line.starts_with('[') {
            if let (Some(path), Some(url)) = (path.take(), url.take()) {
                result.push((path, url));
            }
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "path" => path = Some(value.trim().to_string()),
                "url" => url = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    result
}

fn package_error(error: impl std::fmt::Display) -> fbuild_core::FbuildError {
    fbuild_core::FbuildError::PackageError(error.to_string())
}

/// Every plan entry must name a path `.gitmodules` declares and that
/// extracted empty; anything else means upstream moved on.
fn check_plan(
    package: &str,
    root: &Path,
    declared: &[String],
    plan: &SubmodulePlan,
) -> fbuild_core::Result<()> {
    let sources = plan
        .sources
        .iter()
        .map(|s| (s.path.as_str(), "pinned source"));
    let expected = plan
        .expected_empty
        .iter()
        .map(|e| (e.path.as_str(), "expected empty"));
    for (path, kind) in sources.chain(expected) {
        if !declared.iter().any(|d| d == path) {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "{package} lists submodule `{path}` ({kind}), but its .gitmodules \
                 does not declare that path. The entry is stale: remove it, or \
                 move it to the path upstream uses now."
            )));
        }
        if !is_empty_dir(&root.join(path)) {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "{package} lists submodule `{path}` ({kind}), but the archive did \
                 not ship that directory empty. If upstream now bundles it, the \
                 entry is stale and should be removed."
            )));
        }
    }
    Ok(())
}

/// Empty submodules the plan does not excuse.
fn unexpected_empty_submodules(root: &Path, plan: &SubmodulePlan) -> Vec<EmptySubmodule> {
    find_empty_submodules(root)
        .into_iter()
        .filter(|empty| {
            !plan
                .expected_empty
                .iter()
                .any(|expected| expected.path == empty.declared_path)
        })
        .collect()
}

/// Fill each pinned submodule under `root` from its archive.
async fn populate(
    package: &str,
    root: &Path,
    sources: &[SubmoduleSource],
) -> fbuild_core::Result<()> {
    for source in sources {
        let dest = root.join(&source.path);
        let work = dest.with_file_name(format!(
            "{}.fbuild-fetch",
            dest.file_name().unwrap_or_default().to_string_lossy()
        ));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        let fetched = fetch_into(source, &work, &dest).await;
        let _ = std::fs::remove_dir_all(&work);
        fetched.map_err(|e| {
            fbuild_core::FbuildError::PackageError(format!(
                "{package}: fetching submodule `{}` from {} failed: {e}",
                source.path, source.url
            ))
        })?;
        tracing::info!(
            "{package}: populated submodule {} from {}",
            source.path,
            source.url
        );
    }
    Ok(())
}

async fn fetch_into(source: &SubmoduleSource, work: &Path, dest: &Path) -> fbuild_core::Result<()> {
    let archive = crate::downloader::download_file(&source.url, work).await?;
    if let Some(sha256) = &source.sha256 {
        crate::downloader::verify_checksum(&archive, sha256)?;
    }
    let extracted = work.join("extracted");
    std::fs::create_dir_all(&extracted)?;
    crate::extractor::extract(&archive, &extracted)?;
    move_archive_contents(&extracted, dest)?;
    Ok(())
}

/// Move an extracted archive's contents into `dest`, looking through the
/// single top-level directory GitHub archives wrap everything in
/// (`Adafruit_TinyUSB_Arduino-<sha>/`).
fn move_archive_contents(extracted: &Path, dest: &Path) -> std::io::Result<()> {
    let entries = std::fs::read_dir(extracted)?.collect::<std::io::Result<Vec<_>>>()?;
    let top = match entries.as_slice() {
        [only] if only.file_type()?.is_dir() => only.path(),
        _ => extracted.to_path_buf(),
    };
    for entry in std::fs::read_dir(&top)? {
        let entry = entry?;
        std::fs::rename(entry.path(), dest.join(entry.file_name()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    const ESP8266_GITMODULES: &str = "\
[submodule \"libraries/LittleFS/lib/littlefs\"]
\tpath = libraries/LittleFS/lib/littlefs
\turl = https://github.com/littlefs-project/littlefs.git
[submodule \"libraries/SoftwareSerial\"]
\tpath = libraries/SoftwareSerial
\turl = https://github.com/plerup/espsoftwareserial.git
";

    const CH32V_GITMODULES: &str = "\
[submodule \"libraries/Adafruit_TinyUSB_Arduino\"]
\tpath = libraries/Adafruit_TinyUSB_Arduino
\turl = https://github.com/adafruit/Adafruit_TinyUSB_Arduino.git
";

    const TINYUSB: &str = "libraries/Adafruit_TinyUSB_Arduino";

    #[test]
    fn gitlink_resolution_only_accepts_immutable_github_commit_archives() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert!(is_github_commit_archive_url(&format!(
            "https://github.com/openwch/arduino_core_ch32/archive/{sha}.tar.gz"
        )));
        for url in [
            "https://registry.platformio.org/download/framework.tar.gz",
            "https://github.com/openwch/arduino_core_ch32/archive/main.tar.gz",
            "https://github.com/openwch/arduino_core_ch32/archive/0123456789abcdef0123456789abcdef01234567.tar.gz?unexpected=1",
            "https://attacker@github.com/openwch/arduino_core_ch32/archive/0123456789abcdef0123456789abcdef01234567.tar.gz",
        ] {
            assert!(!is_github_commit_archive_url(url), "{url}");
        }
    }

    #[test]
    fn gitlink_paths_must_stay_inside_extracted_package() {
        let root = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("safe")).unwrap();
        assert_eq!(
            checked_submodule_dest(root.path(), "safe").unwrap(),
            root.path().join("safe").canonicalize().unwrap()
        );
        for path in ["../escape", "/escape", "safe/../escape", "safe\\escape"] {
            assert!(checked_submodule_dest(root.path(), path).is_err(), "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn gitlink_paths_reject_symlink_escape() {
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("outside")).unwrap();
        assert!(checked_submodule_dest(root.path(), "outside").is_err());
    }

    fn tinyusb_plan() -> SubmodulePlan {
        SubmodulePlan {
            sources: vec![SubmoduleSource {
                path: TINYUSB.to_string(),
                url: "https://example.invalid/tinyusb.tar.gz".to_string(),
                sha256: Some("0".repeat(64)),
            }],
            expected_empty: Vec::new(),
            github_gitlinks: false,
        }
    }

    fn expect_empty(path: &str) -> SubmodulePlan {
        SubmodulePlan {
            sources: Vec::new(),
            expected_empty: vec![ExpectedEmpty {
                path: path.to_string(),
                reason: "supplied another way".to_string(),
            }],
            github_gitlinks: false,
        }
    }

    #[test]
    fn declared_paths_are_read_from_gitmodules() {
        assert_eq!(
            declared_submodule_paths(ESP8266_GITMODULES),
            vec![
                "libraries/LittleFS/lib/littlefs".to_string(),
                "libraries/SoftwareSerial".to_string(),
            ]
        );
    }

    /// The exact shape FastLED/fbuild#1380 reported: directories present,
    /// contents absent.
    #[test]
    fn empty_submodule_directories_are_reported() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        write(root, ".gitmodules", ESP8266_GITMODULES);
        std::fs::create_dir_all(root.join("libraries/LittleFS/lib/littlefs")).unwrap();
        std::fs::create_dir_all(root.join("libraries/SoftwareSerial")).unwrap();
        // The vendored sources next to the empty submodule are what made the
        // real failure confusing — lfs.c present, lfs.h absent.
        write(
            root,
            "libraries/LittleFS/src/LittleFS.h",
            "#include \"../lib/littlefs/lfs.h\"",
        );

        let found = find_empty_submodules(root);
        let paths: Vec<&str> = found.iter().map(|e| e.declared_path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "libraries/LittleFS/lib/littlefs",
                "libraries/SoftwareSerial"
            ]
        );
    }

    #[test]
    fn populated_submodules_are_not_reported() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        write(root, ".gitmodules", ESP8266_GITMODULES);
        write(root, "libraries/LittleFS/lib/littlefs/lfs.h", "// header");
        write(
            root,
            "libraries/SoftwareSerial/SoftwareSerial.h",
            "// header",
        );
        assert!(find_empty_submodules(root).is_empty());
    }

    /// Most packages are plain archives, not git checkouts. No `.gitmodules`
    /// is the normal case and must not be treated as a finding.
    #[test]
    fn a_package_without_gitmodules_is_clean() {
        let tmp = tempfile::TempDir::new().unwrap();
        write(tmp.path(), "cores/arduino/main.cpp", "int main(){}");
        assert!(find_empty_submodules(tmp.path()).is_empty());
    }

    /// A declared submodule whose directory is missing entirely is a
    /// different failure — an incomplete extract, not a submodule-less
    /// archive. Reporting it here would send the reader after the wrong
    /// cause.
    #[test]
    fn a_missing_submodule_directory_is_not_claimed_as_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        write(tmp.path(), ".gitmodules", ESP8266_GITMODULES);
        assert!(find_empty_submodules(tmp.path()).is_empty());
    }

    #[test]
    fn plan_entries_for_declared_empty_submodules_are_accepted() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(TINYUSB)).unwrap();
        let declared = declared_submodule_paths(CH32V_GITMODULES);
        check_plan("ch32v-core", tmp.path(), &declared, &tinyusb_plan()).unwrap();
        check_plan("ch32v-core", tmp.path(), &declared, &expect_empty(TINYUSB)).unwrap();
    }

    /// Upstream dropping or moving the submodule must not leave an entry that
    /// silently does nothing.
    #[test]
    fn a_plan_entry_for_an_undeclared_path_is_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(TINYUSB)).unwrap();
        let err = check_plan("ch32v-core", tmp.path(), &[], &tinyusb_plan())
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not declare"), "{err}");
    }

    /// Upstream starting to bundle the contents must neither be overwritten
    /// by an older pin nor excused by an outdated expected-empty entry.
    #[test]
    fn a_plan_entry_for_a_populated_submodule_is_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        write(
            tmp.path(),
            "libraries/Adafruit_TinyUSB_Arduino/src/Adafruit_TinyUSB.h",
            "// header",
        );
        let declared = declared_submodule_paths(CH32V_GITMODULES);
        for plan in [tinyusb_plan(), expect_empty(TINYUSB)] {
            let err = check_plan("ch32v-core", tmp.path(), &declared, &plan)
                .unwrap_err()
                .to_string();
            assert!(err.contains("did not ship that directory empty"), "{err}");
        }
    }

    /// An expected-empty entry excuses only its own path, so the #1380 case
    /// stays caught next to it.
    #[test]
    fn expected_empty_submodules_excuse_only_their_own_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        write(root, ".gitmodules", ESP8266_GITMODULES);
        std::fs::create_dir_all(root.join("libraries/LittleFS/lib/littlefs")).unwrap();
        std::fs::create_dir_all(root.join("libraries/SoftwareSerial")).unwrap();

        let found = unexpected_empty_submodules(root, &expect_empty("libraries/SoftwareSerial"));
        let paths: Vec<&str> = found.iter().map(|e| e.declared_path.as_str()).collect();
        assert_eq!(paths, vec!["libraries/LittleFS/lib/littlefs"]);
    }

    /// The plan describes the default archive; an override's commit may
    /// declare different submodules.
    #[test]
    fn an_override_drops_the_submodule_plan() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = PackageBase::with_cache_root(
            "ch32v-core",
            "1.0.4",
            "https://example.invalid/core.tar.gz",
            "https://example.invalid/core.tar.gz",
            None,
            crate::CacheSubdir::Platforms,
            tmp.path(),
            &tmp.path().join("cache"),
        )
        .with_submodule_source(TINYUSB, "https://example.invalid/t.tar.gz", "0")
        .expect_empty_submodule("extra/core-api", "supplied another way");
        assert!(!base.submodules.is_empty());

        let overridden = base.with_override(fbuild_config::PackageOverride {
            url: "https://example.invalid/other.tar.gz".to_string(),
            version: "1.0.4+gabc".to_string(),
            checksum: None,
        });
        assert!(overridden.submodules.is_empty());
    }

    #[test]
    fn archive_contents_are_moved_out_of_the_wrapper_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let extracted = tmp.path().join("extracted");
        write(
            &extracted,
            "Adafruit_TinyUSB_Arduino-1f9da49/src/Adafruit_TinyUSB.h",
            "// header",
        );
        write(
            &extracted,
            "Adafruit_TinyUSB_Arduino-1f9da49/library.properties",
            "name=TinyUSB",
        );
        let dest = tmp.path().join(TINYUSB);
        std::fs::create_dir_all(&dest).unwrap();

        move_archive_contents(&extracted, &dest).unwrap();

        assert!(dest.join("src/Adafruit_TinyUSB.h").is_file());
        assert!(dest.join("library.properties").is_file());
        assert!(!dest.join("Adafruit_TinyUSB_Arduino-1f9da49").exists());
    }

    #[test]
    fn archive_contents_without_a_wrapper_are_moved_as_is() {
        let tmp = tempfile::TempDir::new().unwrap();
        let extracted = tmp.path().join("extracted");
        write(&extracted, "src/Adafruit_TinyUSB.h", "// header");
        write(&extracted, "library.properties", "name=TinyUSB");
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        move_archive_contents(&extracted, &dest).unwrap();

        assert!(dest.join("src/Adafruit_TinyUSB.h").is_file());
        assert!(dest.join("library.properties").is_file());
    }

    #[test]
    fn the_error_names_the_directories_and_the_archive_kind() {
        let empty = vec![EmptySubmodule {
            declared_path: "libraries/LittleFS/lib/littlefs".to_string(),
            extracted_at: NormalizedPath::from("/cache/x/libraries/LittleFS/lib/littlefs"),
        }];
        let msg = empty_submodule_error(
            "esp8266-arduino",
            "https://github.com/esp8266/Arduino/archive/refs/tags/3.1.2.tar.gz",
            &empty,
        );
        assert!(msg.contains("libraries/LittleFS/lib/littlefs"), "{msg}");
        assert!(msg.contains("source archive"), "{msg}");

        let release = empty_submodule_error(
            "esp8266-arduino",
            "https://github.com/esp8266/Arduino/releases/download/3.1.2/esp8266-3.1.2.zip",
            &empty,
        );
        assert!(
            !release.contains("source archive"),
            "a release-asset URL must not be blamed on the archive form: {release}"
        );
    }
}
