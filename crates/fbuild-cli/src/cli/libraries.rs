//! `fbuild libraries`: open the daemon-served Library Manager web page
//! (FastLED/fbuild#1076 Phase 2) in the default browser.
//!
//! Unlike `fbuild plotter` / `fbuild build-progress` / `fbuild boards`,
//! this page is not daemon-global — it needs a project directory and
//! environment to know which `platformio.ini` to read. So this command
//! resolves both (project dir the same way `fbuild build`/`fbuild ide` do:
//! subcommand arg, else the top-level positional, else `.`; environment:
//! explicit `-e`, else `platformio.ini`'s default environment) and passes
//! them through as `?project=&env=` query params to
//! `GET /libraries` (`crates/fbuild-daemon/web/libraries/index.html`),
//! which reads the same params straight back out of its own URL to call
//! `GET /api/ide/libraries`.

use std::path::Path;

use crate::daemon_client;
use crate::output;

use super::build::{normalize_path, open_in_browser};

/// Build the `/libraries?project=&env=` URL for the daemon at `base_url`.
/// Pure — no I/O — so it's directly testable without a running daemon.
/// `project_dir` is expected to already be an absolute, canonicalized path
/// (see [`normalize_path`]) — this function only percent-encodes it.
pub fn libraries_url(base_url: &str, project_dir: &str, environment: &str) -> String {
    format!(
        "{base_url}/libraries?project={}&env={}",
        percent_encode_query(project_dir),
        percent_encode_query(environment)
    )
}

/// Minimal percent-encoding for the handful of characters that show up in
/// an absolute filesystem path (`C:\foo\bar`, `/home/foo/bar`) or an
/// environment name, but aren't safe unescaped in a URL query value.
/// Deliberately small — not a general-purpose encoder (mirrors
/// `cli::plotter::percent_encode_query`).
fn percent_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Resolve the environment to open the Library Manager for: explicit `-e`
/// wins, else `platformio.ini`'s default environment, else an error asking
/// the caller to pass `-e` explicitly. Deliberately does not fall back to
/// a persisted `fbuild ide` choice or a bare `"default"` string — an
/// environment that doesn't exist in `platformio.ini` would just bounce
/// off the daemon endpoint's validation with a less helpful error.
fn resolve_libraries_env(
    project_dir: &Path,
    explicit: Option<&str>,
) -> fbuild_core::Result<String> {
    if let Some(env) = explicit {
        return Ok(env.to_string());
    }
    let ini_path = project_dir.join("platformio.ini");
    let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
    config
        .get_default_environment()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError(
                "no default environment in platformio.ini; pass -e <environment>".into(),
            )
        })
}

/// `fbuild libraries [<project_dir>] [-e <environment>]`
pub async fn run_libraries(
    project_dir: String,
    environment: Option<String>,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;

    let normalized = normalize_path(&project_dir).await?;
    let env_name = resolve_libraries_env(Path::new(&normalized), environment.as_deref())?;

    let base_url = fbuild_paths::get_daemon_url();
    let url = libraries_url(&base_url, &normalized, &env_name);

    output::progress(format!("Opening Library Manager: {}", url));
    if let Err(e) = open_in_browser(&url).await {
        output::warn(format!("failed to open browser: {}", e));
        output::warn(format!("open this URL manually: {}", url));
    }
    output::result(url);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libraries_url_encodes_windows_path_and_env() {
        assert_eq!(
            libraries_url("http://127.0.0.1:49200", r"C:\work\fastled", "uno"),
            "http://127.0.0.1:49200/libraries?project=C%3A%5Cwork%5Cfastled&env=uno"
        );
    }

    #[test]
    fn libraries_url_encodes_unix_path() {
        assert_eq!(
            libraries_url("http://127.0.0.1:49200", "/home/foo/proj", "esp32dev"),
            "http://127.0.0.1:49200/libraries?project=%2Fhome%2Ffoo%2Fproj&env=esp32dev"
        );
    }

    #[test]
    fn percent_encode_query_leaves_safe_chars_alone() {
        assert_eq!(percent_encode_query("abc-DEF_123.~"), "abc-DEF_123.~");
    }

    #[test]
    fn resolve_libraries_env_prefers_explicit() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert_eq!(
            resolve_libraries_env(tmp.path(), Some("uno")).unwrap(),
            "uno"
        );
    }

    #[test]
    fn resolve_libraries_env_falls_back_to_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert_eq!(resolve_libraries_env(tmp.path(), None).unwrap(), "uno");
    }

    #[test]
    fn resolve_libraries_env_errors_without_platformio_ini() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_libraries_env(tmp.path(), None).is_err());
    }
}
