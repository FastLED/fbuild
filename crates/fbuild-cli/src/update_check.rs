//! Passive update-check module (FastLED/fbuild#626 Phase 1).
//!
//! Classifies the running binary's install source, checks PyPI or GitHub for
//! a newer stable release, caches the result with a 24-hour TTL, and prints
//! a one-line stderr warning when the current version is stale. Purely
//! passive — never mutates the install, never blocks the command that
//! triggered it, and never fails a build/deploy/monitor invocation.
//!
//! # Detection chain (install source)
//!
//! 1. `FBUILD_INSTALL_SOURCE` env var — explicit override (any of the string
//!    variants of [`InstallSource`]).
//! 2. Filesystem probe around `env::current_exe()`:
//!    - Ancestor dir contains a `fbuild-*.dist-info/direct_url.json` whose
//!      `dir_info.editable == true` or `url` starts with `file://` →
//!      [`InstallSource::LocalSource`].
//!    - Ancestor dir contains a `fbuild-*.dist-info/direct_url.json` with a
//!      VCS payload (`vcs_info.vcs == "git"` etc.) → [`InstallSource::Vcs`].
//!    - Ancestor dir contains a `fbuild-*.dist-info/` (any of `RECORD`,
//!      `METADATA`, `WHEEL`) but no `direct_url.json` → [`InstallSource::Pypi`].
//!    - Ancestor dir contains a `pyvenv.cfg` (virtualenv marker) →
//!      [`InstallSource::Pypi`] (weaker signal; assumes wheel install).
//! 3. Fallback: [`InstallSource::DirectGithub`] — assume the user grabbed a
//!    native binary off a GitHub release. Overridden by the `FBUILD_INSTALL_SOURCE`
//!    env var when we're really uncertain.
//!
//! # Suppression order
//!
//! - `FBUILD_NO_UPDATE_CHECK` env set to non-empty non-`0` value → skip.
//! - `--no-update-check` CLI flag → skip.
//! - `CI` / `GITHUB_ACTIONS` env indicates CI → skip.
//! - Cache says the last check was within TTL and reported not-stale → skip
//!   HTTP but still return the cached result (no warning printed).
//!
//! # Non-fatal
//!
//! Every fallible operation returns `Result<..., UpdateCheckError>` and the
//! top-level [`run_passive_check`] SWALLOWS all errors with a single
//! `tracing::debug!` line. Network failures never surface to the user or
//! command exit code.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, SystemTime};

use fbuild_core::path::NormalizedPath;

/// Where the running `fbuild` binary lives, per the schema in issue #626.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallSource {
    /// Installed via `pip install fbuild` from PyPI (or `pip install .` /
    /// `pip install <wheel>` where the wheel came from a normal PyPI-shaped
    /// build). Update source is PyPI.
    Pypi,
    /// `pip install -e <path>` or a source install where `direct_url.json`
    /// says the origin was a local directory. Suggest `git pull` +
    /// `pip install -e .` instead of any automatic update.
    LocalSource,
    /// `pip install git+https://...` — `direct_url.json` has a `vcs_info`
    /// payload. Suggest re-installing from the same URL.
    Vcs,
    /// Native binary dropped by the user (or setup-fbuild action) from a
    /// GitHub release archive. Update source is GitHub releases.
    DirectGithub,
    /// Detection failed. Fall back to GitHub releases (safe: PyPI users
    /// get a GitHub link; slightly worse UX than the correct pip command
    /// but the link works).
    Unknown,
}

impl InstallSource {
    /// Parse the `FBUILD_INSTALL_SOURCE` env-var value.
    fn from_env_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pypi" | "pip" => Some(Self::Pypi),
            "local" | "local-source" | "editable" => Some(Self::LocalSource),
            "vcs" | "git" => Some(Self::Vcs),
            "direct" | "direct-github" | "github" | "release" => Some(Self::DirectGithub),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    fn is_stable_pypi_source(self) -> bool {
        matches!(self, Self::Pypi)
    }

    fn is_github_source(self) -> bool {
        matches!(self, Self::DirectGithub | Self::Unknown)
    }

    /// Human-readable suggestion the update warning appends.
    fn suggestion(self, latest: &str) -> String {
        match self {
            Self::Pypi => "run: python -m pip install --upgrade fbuild".to_string(),
            Self::LocalSource => "editable install — run: git pull && pip install -e .".to_string(),
            Self::Vcs => {
                "VCS install — re-run: pip install --upgrade git+https://github.com/FastLED/fbuild"
                    .to_string()
            }
            Self::DirectGithub | Self::Unknown => format!(
                "download: https://github.com/FastLED/fbuild/releases/tag/v{}",
                latest
            ),
        }
    }
}

/// Cached result of the last successful update check. Written to
/// `<cache_dir>/update_check.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CachedCheck {
    pub checked_at_epoch_secs: u64,
    pub install_source: InstallSource,
    pub current_version: String,
    pub latest_version: String,
    pub stale: bool,
    pub check_url: String,
    /// Cache TTL in seconds — persisted so a future refactor can change the
    /// default without invalidating all existing cache files at once.
    pub ttl_secs: u64,
}

impl CachedCheck {
    fn is_fresh(&self, now_epoch: u64) -> bool {
        now_epoch.saturating_sub(self.checked_at_epoch_secs) < self.ttl_secs
    }
}

/// Errors from the internal steps. The public [`run_passive_check`] never
/// returns these to the caller — they surface only via `tracing::debug!`.
#[derive(Debug)]
pub(crate) enum UpdateCheckError {
    Http(String),
    Parse(String),
    Semver(String),
    Cache(String),
}

impl std::fmt::Display for UpdateCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(m) => write!(f, "http error: {m}"),
            Self::Parse(m) => write!(f, "parse error: {m}"),
            Self::Semver(m) => write!(f, "semver error: {m}"),
            Self::Cache(m) => write!(f, "cache error: {m}"),
        }
    }
}

/// Public options — passed by the CLI's top-level command dispatch.
#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    /// `--no-update-check` flag from `Cli`.
    pub no_update_check: bool,
}

const CACHE_FILENAME: &str = "update_check.json";
const DEFAULT_TTL_SECS: u64 = 24 * 60 * 60; // 24 hours per issue's default suggestion.
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);
const USER_AGENT: &str = concat!("fbuild-cli/", env!("CARGO_PKG_VERSION"));

/// Top-level entry point. Non-blocking on all paths that matter — completes
/// synchronously in the reactor if the cache is hot, else spends up to 3 s
/// on the network. Returns without emitting anything (except optionally the
/// stderr warning) — callers proceed to their real work immediately.
///
/// Call from a command's early setup, before the "real" work starts, so the
/// warning appears at the top of the operator's console output rather than
/// buried in build logs.
pub async fn run_passive_check(current_version: &str, opts: &CheckOptions) {
    if let Err(e) = try_run_passive_check(current_version, opts).await {
        tracing::debug!("update check skipped: {e}");
    }
}

async fn try_run_passive_check(
    current_version: &str,
    opts: &CheckOptions,
) -> Result<(), UpdateCheckError> {
    // --- 1. Suppression checks (never network, never cache) ---
    if opts.no_update_check {
        return Ok(());
    }
    if suppress_from_env() {
        return Ok(());
    }

    // --- 2. Install source classification ---
    let source = classify_install_source();

    // --- 3. Cache hit path ---
    let cache_path = cache_file_path();
    let now = now_epoch_secs();
    if let Ok(cached) = read_cache(&cache_path) {
        if cached.is_fresh(now) && cached.current_version == current_version {
            if cached.stale {
                emit_warning(
                    current_version,
                    &cached.latest_version,
                    cached.install_source,
                );
            }
            return Ok(());
        }
    }

    // --- 4. Network check ---
    let (latest, check_url) = fetch_latest_for_source(source).await?;

    // --- 5. Compare (semver, prerelease-safe) ---
    let stale = is_newer(&latest, current_version)?;

    // --- 6. Persist cache ---
    let cached = CachedCheck {
        checked_at_epoch_secs: now,
        install_source: source,
        current_version: current_version.to_string(),
        latest_version: latest.clone(),
        stale,
        check_url,
        ttl_secs: DEFAULT_TTL_SECS,
    };
    let _ = write_cache(&cache_path, &cached);

    // --- 7. Emit if stale ---
    if stale {
        emit_warning(current_version, &latest, source);
    }

    Ok(())
}

// --------------------------------------------------------------------------
// Suppression / classification
// --------------------------------------------------------------------------

fn suppress_from_env() -> bool {
    if is_truthy_env("FBUILD_NO_UPDATE_CHECK") {
        return true;
    }
    // CI auto-skip — the issue's open question lists this as reasonable default.
    if is_ci_env() {
        return true;
    }
    false
}

/// True when the env var is set to a truthy value. Empty, `0`, and any
/// case-fold of `false` are treated as falsy — matches the convention
/// PowerShell / GitHub Actions / bash all seem to converge on.
fn is_truthy_env(key: &str) -> bool {
    let Ok(v) = std::env::var(key) else {
        return false;
    };
    let trimmed = v.trim();
    if trimmed.is_empty() || trimmed == "0" || trimmed.eq_ignore_ascii_case("false") {
        return false;
    }
    true
}

fn is_ci_env() -> bool {
    // Common CI markers. `CI=true` is universal per
    // https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/store-information-in-variables.
    [
        "CI",
        "GITHUB_ACTIONS",
        "GITLAB_CI",
        "CIRCLECI",
        "JENKINS_URL",
    ]
    .iter()
    .any(|k| is_truthy_env(k))
}

/// Classify the running binary's install source. Never panics; returns
/// [`InstallSource::Unknown`] as the terminal fallback.
pub fn classify_install_source() -> InstallSource {
    // Explicit override wins.
    if let Ok(v) = std::env::var("FBUILD_INSTALL_SOURCE") {
        if let Some(parsed) = InstallSource::from_env_str(&v) {
            return parsed;
        }
    }

    let Ok(exe) = std::env::current_exe() else {
        return InstallSource::Unknown;
    };

    classify_from_exe_path(&exe)
}

/// Filesystem-probe classification, factored so unit tests can point it at
/// a tempdir-built fake install tree.
fn classify_from_exe_path(exe: &Path) -> InstallSource {
    // Walk ancestors looking for pip's dist-info marker or a pyvenv.cfg.
    let mut ancestor = exe.parent();
    let mut depth = 0;
    while let Some(dir) = ancestor {
        depth += 1;
        if depth > 8 {
            break; // Avoid climbing to filesystem root on weird layouts.
        }

        // Look for an fbuild dist-info under this dir OR one level deeper.
        if let Some(source) = probe_dist_info(dir) {
            return source;
        }

        // `pyvenv.cfg` next to the Scripts/bin dir → virtualenv (assume PyPI wheel).
        if dir.join("pyvenv.cfg").is_file() {
            return InstallSource::Pypi;
        }

        ancestor = dir.parent();
    }
    InstallSource::DirectGithub
}

/// Look for `fbuild-*.dist-info` directly under `dir` and under
/// `dir/site-packages` (the venv layout). Returns the resolved source
/// classification when found.
fn probe_dist_info(dir: &Path) -> Option<InstallSource> {
    for site in [
        dir.to_path_buf(),
        dir.join("site-packages"),
        dir.join("Lib").join("site-packages"),
        dir.join("lib").join("site-packages"),
    ] {
        if !site.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&site) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if !name_str.starts_with("fbuild-") || !name_str.ends_with(".dist-info") {
                continue;
            }
            let dist = entry.path();
            let direct_url = dist.join("direct_url.json");
            if direct_url.is_file() {
                return Some(classify_direct_url(&direct_url));
            }
            return Some(InstallSource::Pypi);
        }
    }
    None
}

fn classify_direct_url(path: &Path) -> InstallSource {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return InstallSource::Pypi;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return InstallSource::Pypi;
    };
    // Per PEP 610, `direct_url.json` carries either `vcs_info`, `archive_info`,
    // or `dir_info` (with optional `editable: bool`) plus a `url`.
    if v.get("vcs_info").is_some() {
        return InstallSource::Vcs;
    }
    if let Some(dir_info) = v.get("dir_info") {
        if dir_info
            .get("editable")
            .and_then(|e| e.as_bool())
            .unwrap_or(false)
        {
            return InstallSource::LocalSource;
        }
    }
    if let Some(url) = v.get("url").and_then(|u| u.as_str()) {
        if url.starts_with("file://") {
            return InstallSource::LocalSource;
        }
    }
    InstallSource::Pypi
}

// --------------------------------------------------------------------------
// Network — PyPI + GitHub
// --------------------------------------------------------------------------

async fn fetch_latest_for_source(
    source: InstallSource,
) -> Result<(String, String), UpdateCheckError> {
    if source.is_stable_pypi_source()
        || matches!(source, InstallSource::LocalSource | InstallSource::Vcs)
    {
        // Editable / VCS installs still get the PyPI version as the target
        // (the user's env has fbuild-the-package; the "latest published" is
        // the meaningful comparison).
        fetch_latest_pypi().await
    } else if source.is_github_source() {
        fetch_latest_github().await
    } else {
        Err(UpdateCheckError::Http("no source".into()))
    }
}

async fn fetch_latest_pypi() -> Result<(String, String), UpdateCheckError> {
    // PyPI Simple Index JSON per https://docs.pypi.org/api/index-api/. The
    // deprecated `/pypi/<pkg>/json` still works but the spec docs push us at
    // the Simple JSON for integrations.
    let url = "https://pypi.org/simple/fbuild/";
    let client = fbuild_core::http::client_with_timeout(HTTP_TIMEOUT);
    let resp = client
        .get(url)
        .header("Accept", "application/vnd.pypi.simple.v1+json")
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|e| UpdateCheckError::Http(format!("PyPI GET: {e}")))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| UpdateCheckError::Parse(format!("PyPI JSON: {e}")))?;
    let versions = body
        .get("versions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| UpdateCheckError::Parse("PyPI: missing `versions` array".into()))?;
    // The array is version-sorted but the spec doesn't guarantee order;
    // pick the max via semver comparison, skipping prereleases.
    let latest = pick_latest_stable_from_strings(versions.iter().filter_map(|v| v.as_str()))
        .ok_or_else(|| UpdateCheckError::Parse("PyPI: no stable version found".into()))?;
    Ok((latest, url.to_string()))
}

async fn fetch_latest_github() -> Result<(String, String), UpdateCheckError> {
    // GitHub REST /releases/latest → the latest non-draft, non-prerelease
    // release, exactly what we want by default. Unauthenticated is fine for
    // public repos (60 requests/hour/IP; cache keeps us well under that).
    let url = "https://api.github.com/repos/FastLED/fbuild/releases/latest";
    let client = fbuild_core::http::client_with_timeout(HTTP_TIMEOUT);
    let resp = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|e| UpdateCheckError::Http(format!("GitHub GET: {e}")))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| UpdateCheckError::Parse(format!("GitHub JSON: {e}")))?;
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| UpdateCheckError::Parse("GitHub: missing `tag_name`".into()))?;
    let stripped = tag.strip_prefix('v').unwrap_or(tag);
    Ok((stripped.to_string(), url.to_string()))
}

/// Given an iterator over PyPI version strings, return the highest STABLE
/// (no prerelease suffix) version. Skips anything semver can't parse.
fn pick_latest_stable_from_strings<'a, I>(iter: I) -> Option<String>
where
    I: Iterator<Item = &'a str>,
{
    let mut best: Option<semver::Version> = None;
    for s in iter {
        let Ok(v) = semver::Version::parse(s) else {
            continue;
        };
        if !v.pre.is_empty() {
            continue;
        }
        match &best {
            None => best = Some(v),
            Some(current) if v > *current => best = Some(v),
            _ => {}
        }
    }
    best.map(|v| v.to_string())
}

// --------------------------------------------------------------------------
// Semver comparison
// --------------------------------------------------------------------------

fn is_newer(latest: &str, current: &str) -> Result<bool, UpdateCheckError> {
    let latest = semver::Version::parse(latest)
        .map_err(|e| UpdateCheckError::Semver(format!("latest '{latest}': {e}")))?;
    let current = semver::Version::parse(current)
        .map_err(|e| UpdateCheckError::Semver(format!("current '{current}': {e}")))?;

    // Per acceptance criteria: prereleases are only considered "newer" if the
    // current version is itself a prerelease. Stable → stable comparison is
    // straightforward.
    if !latest.pre.is_empty() && current.pre.is_empty() {
        return Ok(false);
    }
    Ok(latest > current)
}

// --------------------------------------------------------------------------
// Cache I/O
// --------------------------------------------------------------------------

fn cache_file_path() -> NormalizedPath {
    NormalizedPath::new(fbuild_paths::get_cache_root().join(CACHE_FILENAME))
}

fn read_cache(path: &Path) -> Result<CachedCheck, UpdateCheckError> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| UpdateCheckError::Cache(format!("read: {e}")))?;
    serde_json::from_str(&raw).map_err(|e| UpdateCheckError::Cache(format!("parse: {e}")))
}

fn write_cache(path: &Path, cache: &CachedCheck) -> Result<(), UpdateCheckError> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let raw = serde_json::to_string_pretty(cache)
        .map_err(|e| UpdateCheckError::Cache(format!("encode: {e}")))?;
    std::fs::write(path, raw).map_err(|e| UpdateCheckError::Cache(format!("write: {e}")))
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// --------------------------------------------------------------------------
// User-visible warning
// --------------------------------------------------------------------------

fn emit_warning(current: &str, latest: &str, source: InstallSource) {
    // Concise, one-line, stderr-only. Structured enough to grep for in a
    // CI log if someone forgets to set `CI=true`.
    eprintln!(
        "fbuild {} → {} available ({}). {}",
        current,
        latest,
        install_source_display(source),
        source.suggestion(latest)
    );
}

fn install_source_display(s: InstallSource) -> &'static str {
    match s {
        InstallSource::Pypi => "PyPI",
        InstallSource::LocalSource => "local editable",
        InstallSource::Vcs => "VCS",
        InstallSource::DirectGithub => "GitHub release",
        InstallSource::Unknown => "unknown source",
    }
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn install_source_env_parsing() {
        assert_eq!(
            InstallSource::from_env_str("pypi"),
            Some(InstallSource::Pypi)
        );
        assert_eq!(
            InstallSource::from_env_str("PIP"),
            Some(InstallSource::Pypi)
        );
        assert_eq!(
            InstallSource::from_env_str(" local "),
            Some(InstallSource::LocalSource)
        );
        assert_eq!(
            InstallSource::from_env_str("editable"),
            Some(InstallSource::LocalSource)
        );
        assert_eq!(InstallSource::from_env_str("git"), Some(InstallSource::Vcs));
        assert_eq!(
            InstallSource::from_env_str("release"),
            Some(InstallSource::DirectGithub)
        );
        assert_eq!(
            InstallSource::from_env_str("github"),
            Some(InstallSource::DirectGithub)
        );
        assert_eq!(InstallSource::from_env_str("garbage"), None);
    }

    #[test]
    fn is_newer_stable_pair() {
        assert!(is_newer("2.3.16", "2.3.15").unwrap());
        assert!(!is_newer("2.3.15", "2.3.15").unwrap());
        assert!(!is_newer("2.3.15", "2.3.16").unwrap());
        assert!(is_newer("3.0.0", "2.99.99").unwrap());
    }

    #[test]
    fn is_newer_prerelease_hidden_from_stable() {
        // Current is stable, "latest" is a prerelease — not stale.
        assert!(!is_newer("2.4.0-rc.1", "2.3.15").unwrap());
    }

    #[test]
    fn is_newer_prerelease_visible_from_prerelease() {
        // Current is itself a prerelease — we DO show newer prereleases.
        assert!(is_newer("2.4.0-rc.2", "2.4.0-rc.1").unwrap());
    }

    #[test]
    fn pypi_pick_latest_stable_skips_prereleases() {
        let versions = [
            "2.3.14",
            "2.3.15",
            "2.4.0-rc.1",
            "2.4.0-alpha.1",
            "2.3.15.dev0",
        ];
        assert_eq!(
            pick_latest_stable_from_strings(versions.iter().copied()),
            Some("2.3.15".to_string())
        );
    }

    #[test]
    fn pypi_pick_latest_stable_all_prereleases_returns_none() {
        let versions = ["2.4.0-rc.1", "2.4.0-alpha.1"];
        assert_eq!(
            pick_latest_stable_from_strings(versions.iter().copied()),
            None
        );
    }

    #[test]
    fn cache_fresh_within_ttl() {
        let cached = CachedCheck {
            checked_at_epoch_secs: 1_000_000,
            install_source: InstallSource::Pypi,
            current_version: "2.3.15".into(),
            latest_version: "2.3.15".into(),
            stale: false,
            check_url: "https://pypi.org/simple/fbuild/".into(),
            ttl_secs: 3600,
        };
        assert!(cached.is_fresh(1_000_500));
        assert!(!cached.is_fresh(1_100_000));
    }

    #[test]
    fn install_source_suggestion_pypi_uses_pip_upgrade() {
        let msg = InstallSource::Pypi.suggestion("2.3.16");
        assert!(msg.contains("pip install --upgrade fbuild"));
    }

    #[test]
    fn install_source_suggestion_github_uses_release_url() {
        let msg = InstallSource::DirectGithub.suggestion("2.3.16");
        assert!(msg.contains("github.com/FastLED/fbuild/releases/tag/v2.3.16"));
    }

    #[test]
    fn install_source_suggestion_local_editable_says_git_pull() {
        let msg = InstallSource::LocalSource.suggestion("2.3.16");
        assert!(msg.contains("git pull"));
        assert!(msg.contains("pip install -e ."));
    }

    #[test]
    fn classify_from_exe_pyvenv_marker_gives_pypi() {
        let tmp = tempdir().unwrap();
        // Simulate: <venv>/Scripts/fbuild.exe next to <venv>/pyvenv.cfg
        let venv = tmp.path();
        let scripts = venv.join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin").unwrap();
        let exe = scripts.join("fbuild");
        std::fs::write(&exe, "").unwrap();
        assert_eq!(classify_from_exe_path(&exe), InstallSource::Pypi);
    }

    #[test]
    fn classify_from_exe_no_python_context_gives_direct_github() {
        // Binary in a plain dir with no venv marker anywhere → assume the
        // user downloaded it from a GitHub release.
        let tmp = tempdir().unwrap();
        let exe = tmp.path().join("fbuild");
        std::fs::write(&exe, "").unwrap();
        assert_eq!(classify_from_exe_path(&exe), InstallSource::DirectGithub);
    }

    #[test]
    fn classify_from_exe_direct_url_editable_gives_local_source() {
        let tmp = tempdir().unwrap();
        let venv = tmp.path();
        let scripts = venv.join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        // Windows layout: dist-info directly under venv root
        let dist = venv.join("fbuild-2.3.15.dist-info");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("METADATA"), "Name: fbuild\n").unwrap();
        std::fs::write(
            dist.join("direct_url.json"),
            r#"{"url":"file:///home/dev/fbuild","dir_info":{"editable":true}}"#,
        )
        .unwrap();
        let exe = scripts.join("fbuild");
        std::fs::write(&exe, "").unwrap();
        assert_eq!(classify_from_exe_path(&exe), InstallSource::LocalSource);
    }

    #[test]
    fn classify_from_exe_direct_url_vcs_gives_vcs() {
        let tmp = tempdir().unwrap();
        let venv = tmp.path();
        let scripts = venv.join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        let dist = venv.join("fbuild-2.3.15.dist-info");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(
            dist.join("direct_url.json"),
            r#"{"url":"https://github.com/FastLED/fbuild","vcs_info":{"vcs":"git","commit_id":"abc"}}"#,
        )
        .unwrap();
        let exe = scripts.join("fbuild");
        std::fs::write(&exe, "").unwrap();
        assert_eq!(classify_from_exe_path(&exe), InstallSource::Vcs);
    }

    #[test]
    fn classify_from_exe_dist_info_without_direct_url_gives_pypi() {
        let tmp = tempdir().unwrap();
        let venv = tmp.path();
        let scripts = venv.join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        let dist = venv.join("fbuild-2.3.15.dist-info");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("METADATA"), "Name: fbuild\n").unwrap();
        // No direct_url.json — canonical PyPI wheel install shape.
        let exe = scripts.join("fbuild");
        std::fs::write(&exe, "").unwrap();
        assert_eq!(classify_from_exe_path(&exe), InstallSource::Pypi);
    }

    #[test]
    fn cache_roundtrip() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("update_check.json");
        let cached = CachedCheck {
            checked_at_epoch_secs: 1_700_000_000,
            install_source: InstallSource::Pypi,
            current_version: "2.3.15".into(),
            latest_version: "2.3.16".into(),
            stale: true,
            check_url: "https://pypi.org/simple/fbuild/".into(),
            ttl_secs: DEFAULT_TTL_SECS,
        };
        write_cache(&path, &cached).unwrap();
        let read = read_cache(&path).unwrap();
        assert_eq!(read.install_source, InstallSource::Pypi);
        assert_eq!(read.current_version, "2.3.15");
        assert_eq!(read.latest_version, "2.3.16");
        assert!(read.stale);
        assert_eq!(read.ttl_secs, DEFAULT_TTL_SECS);
    }

    #[test]
    fn ci_detection_via_ci_env() {
        // Snapshot + clear ALL CI markers we recognize, not just CI —
        // otherwise GitHub Actions' own GITHUB_ACTIONS=true poisons the
        // `assert!(!is_ci_env())` checks below.
        const CI_KEYS: &[&str] = &[
            "CI",
            "GITHUB_ACTIONS",
            "GITLAB_CI",
            "CIRCLECI",
            "JENKINS_URL",
        ];
        let saved: Vec<(&str, Option<String>)> = CI_KEYS
            .iter()
            .map(|k| (*k, std::env::var(*k).ok()))
            .collect();
        // SAFETY: single-threaded test process.
        for k in CI_KEYS {
            std::env::remove_var(k);
        }
        std::env::set_var("CI", "true");
        assert!(is_ci_env());
        std::env::set_var("CI", "0");
        assert!(!is_ci_env());
        std::env::set_var("CI", "false");
        assert!(!is_ci_env());
        std::env::remove_var("CI");
        assert!(!is_ci_env());
        // Restore.
        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}
