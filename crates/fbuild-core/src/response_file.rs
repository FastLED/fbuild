//! GCC response file utilities shared across all fbuild crates.
//!
//! Handles writing `@file` response files for GCC/G++ on Windows where
//! command-line length limits (32KB CreateProcess) and MSYS2 path translation
//! issues require special handling.
//!
//! Async surface (#813): `write_response_file` is now an `async fn` —
//! it's called once per compile/link, often interleaved with other I/O,
//! so going through `tokio::fs` keeps the runtime free to schedule
//! other work. `write_response_file_blocking` exists as the escape
//! hatch for the (rare) sync call sites.

use crate::Result;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const RESPONSE_FILE_PREFIX: &str = "fbuild_";
const RESPONSE_FILE_SUFFIX: &str = ".rsp";
const RESPONSE_FILE_STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Get the platform-appropriate temp directory for response files.
///
/// On MSYS2/Git Bash, `std::env::temp_dir()` returns `/tmp/` which native
/// Windows GCC treats as `C:\tmp\`. Use an app-owned directory under
/// `~/.fbuild/{dev|prod}/tmp/response-files` instead.
pub fn windows_temp_dir() -> PathBuf {
    if crate::platform::host::is_windows() {
        response_files_dir()
    } else {
        std::env::temp_dir()
    }
}

fn response_files_dir() -> PathBuf {
    response_files_root(&home_dir(), is_dev_mode())
}

fn response_files_root(home: &Path, dev_mode: bool) -> PathBuf {
    let mode = if dev_mode { "dev" } else { "prod" };
    home.join(crate::path::FBUILD_DIR_NAME)
        .join(mode)
        .join("tmp")
        .join("response-files")
}

/// Resolve the home directory through the neutral host facade.
///
/// Deliberately not a local `HOME`-first lookup. On Windows the facade
/// prefers `%USERPROFILE%`, and preferring `HOME` instead put response files
/// under an MSYS path like `/c/Users/you` whenever fbuild ran from Git Bash —
/// which native Windows GCC cannot open, and is precisely the failure
/// [`windows_temp_dir`] exists to avoid (FastLED/fbuild#1349).
fn home_dir() -> PathBuf {
    crate::platform::host::home_dir()
        .map(|home| home.into_path_buf())
        .unwrap_or_else(std::env::temp_dir)
}

fn is_dev_mode() -> bool {
    std::env::var("FBUILD_DEV_MODE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn cleanup_stale_response_files(
    temp_dir: &Path,
    stale_after: Duration,
    now: SystemTime,
) -> std::io::Result<usize> {
    let mut removed = 0usize;
    let entries = match std::fs::read_dir(temp_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_name = match path.file_name().and_then(|s| s.to_str()) {
            Some(name) => name,
            None => continue,
        };
        if !file_name.starts_with(RESPONSE_FILE_PREFIX)
            || !file_name.ends_with(RESPONSE_FILE_SUFFIX)
        {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(_) => continue,
        };
        let age = match now.duration_since(modified) {
            Ok(age) => age,
            Err(_) => continue,
        };
        if age >= stale_after && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }

    Ok(removed)
}

/// Replace backslashes with forward slashes for GCC response files,
/// but preserve `\"` sequences which are intentional escapes in define values.
pub fn replace_path_backslashes(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut result = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'"' {
            result.push('\\');
            result.push('"');
            i += 2;
        } else if bytes[i] == b'\\' {
            result.push('/');
            i += 1;
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

/// Build the deterministic on-disk path + content for a response file
/// without performing any I/O. Shared by both the async and blocking
/// public surfaces.
fn render_response_file(flags: &[String], temp_dir: &Path, prefix: &str) -> (PathBuf, String) {
    // GCC treats backslashes in response files as escape characters
    // (\n = newline, \f = formfeed, etc.). Convert to forward slashes
    // for Windows path compatibility, but preserve \" sequences which
    // are intentional escape sequences (e.g.,
    // -DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\").
    //
    // Flags containing quoted define values need single-quote wrapping.
    // Some define sources use escaped quotes (-DFOO=\"bar\"), while
    // data-driven MCU configs can contain bare quotes (-DFOO="bar").
    // Normalize both forms to the response-file spelling GCC preserves
    // as a string literal.
    let content = flags
        .iter()
        .map(|f| {
            let fwd = replace_path_backslashes(f);
            let normalized = if fwd.contains("\\\"") {
                fwd.replace("\\\"", "\"")
            } else {
                fwd
            };
            if normalized.contains('"') {
                format!("'{}'", normalized)
            } else if normalized.contains(' ') {
                format!("\"{}\"", normalized)
            } else {
                normalized
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let hash = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let digest = hasher.finalize();
        format!(
            "{:02x}{:02x}{:02x}{:02x}",
            digest[0], digest[1], digest[2], digest[3]
        )
    };
    let path = temp_dir.join(format!("fbuild_{}_{}.rsp", prefix, hash));
    (path, content)
}

/// Write flags to a temporary GCC response file (`@file` syntax) using
/// async filesystem I/O.
///
/// Returns the path to the response file.
///
/// Flags containing either `\"` or bare `"` in define values are wrapped in
/// single quotes with `\"` converted to plain `"`. GCC's response file
/// parser preserves literal `"` inside single-quoted arguments.
pub async fn write_response_file(
    flags: &[String],
    temp_dir: &Path,
    prefix: &str,
) -> Result<PathBuf> {
    tokio::fs::create_dir_all(temp_dir).await.map_err(|e| {
        crate::FbuildError::BuildFailed(format!(
            "failed to create temp dir {}: {}",
            temp_dir.display(),
            e
        ))
    })?;
    let _ = cleanup_stale_response_files(temp_dir, RESPONSE_FILE_STALE_AFTER, SystemTime::now());

    let (path, content) = render_response_file(flags, temp_dir, prefix);

    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(path);
    }

    tokio::fs::write(&path, content).await.map_err(|e| {
        crate::FbuildError::BuildFailed(format!(
            "failed to write response file {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(path)
}

/// Sync escape hatch for [`write_response_file`]. Useful in tests and
/// sync diagnostic subcommands.
pub fn write_response_file_blocking(
    flags: &[String],
    temp_dir: &Path,
    prefix: &str,
) -> Result<PathBuf> {
    std::fs::create_dir_all(temp_dir).map_err(|e| {
        crate::FbuildError::BuildFailed(format!(
            "failed to create temp dir {}: {}",
            temp_dir.display(),
            e
        ))
    })?;
    let _ = cleanup_stale_response_files(temp_dir, RESPONSE_FILE_STALE_AFTER, SystemTime::now());

    let (path, content) = render_response_file(flags, temp_dir, prefix);

    if path.exists() {
        return Ok(path);
    }

    std::fs::write(&path, content).map_err(|e| {
        crate::FbuildError::BuildFailed(format!(
            "failed to write response file {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replace_path_backslashes_preserves_escaped_quotes() {
        assert_eq!(
            replace_path_backslashes(r#"-DFOO=\"bar\""#),
            r#"-DFOO=\"bar\""#
        );
    }

    #[test]
    fn test_replace_path_backslashes_converts_path_separators() {
        assert_eq!(
            replace_path_backslashes(r"C:\Users\test\include"),
            "C:/Users/test/include"
        );
    }

    #[test]
    fn test_replace_path_backslashes_mixed() {
        assert_eq!(
            replace_path_backslashes(r#"-I C:\path\to\include -DNAME=\"val\""#),
            r#"-I C:/path/to/include -DNAME=\"val\""#
        );
    }

    #[tokio::test]
    async fn test_write_response_file_reuses_same_path_for_same_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let flags = vec!["-O2".to_string(), "-c".to_string(), "src.cpp".to_string()];

        let first = write_response_file(&flags, tmp.path(), "stable")
            .await
            .unwrap();
        let second = write_response_file(&flags, tmp.path(), "stable")
            .await
            .unwrap();

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn test_write_response_file_changes_path_when_content_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let first = write_response_file(&["-O2".to_string()], tmp.path(), "stable")
            .await
            .unwrap();
        let second = write_response_file(&["-O3".to_string()], tmp.path(), "stable")
            .await
            .unwrap();

        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn test_write_response_file_wraps_escaped_quote_defines() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rsp = write_response_file(
            &[r#"-DARDUINO_BOARD=\"ESP32_DEV\""#.to_string()],
            tmp.path(),
            "define",
        )
        .await
        .unwrap();
        let content = tokio::fs::read_to_string(rsp).await.unwrap();
        assert_eq!(content, r#"'-DARDUINO_BOARD="ESP32_DEV"'"#);
    }

    #[tokio::test]
    async fn test_write_response_file_wraps_bare_quote_defines() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rsp = write_response_file(
            &[r#"-DARDUINO_BSP_VERSION="1.6.1""#.to_string()],
            tmp.path(),
            "define",
        )
        .await
        .unwrap();
        let content = tokio::fs::read_to_string(rsp).await.unwrap();
        assert_eq!(content, r#"'-DARDUINO_BSP_VERSION="1.6.1"'"#);
    }

    #[test]
    fn test_write_response_file_blocking_matches_async_path() {
        // The sync escape hatch must produce byte-identical output to
        // the async path so callers can mix them without surprises.
        let tmp = tempfile::TempDir::new().unwrap();
        let flags = vec!["-O2".to_string(), "-DFOO=bar".to_string()];
        let blocking = write_response_file_blocking(&flags, tmp.path(), "stable").unwrap();
        let content = std::fs::read_to_string(&blocking).unwrap();
        let (expected_path, expected_content) = render_response_file(&flags, tmp.path(), "stable");
        assert_eq!(blocking, expected_path);
        assert_eq!(content, expected_content);
    }

    /// FastLED/fbuild#1349: this module resolved the home directory itself,
    /// preferring `HOME` over `USERPROFILE`. The neutral facade prefers
    /// `USERPROFILE` on Windows, and that difference is exactly the case this
    /// module exists for: under MSYS2 / Git Bash `HOME` is a POSIX path such
    /// as `/c/Users/you`, which native Windows GCC cannot open. Preferring it
    /// put response files somewhere no compiler could read — the failure
    /// `windows_temp_dir` was written to avoid, reintroduced by resolving the
    /// home directory a second way.
    ///
    /// The test harness does not inherit Git Bash's `HOME`, so the divergence
    /// is invisible unless the test sets it.
    #[test]
    fn home_dir_prefers_the_native_profile_over_an_msys_home() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if !crate::platform::host::is_windows() {
            return; // `HOME` is the right answer everywhere else
        }

        struct Restore(&'static str, Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                match self.1.take() {
                    Some(v) => std::env::set_var(self.0, v),
                    None => std::env::remove_var(self.0),
                }
            }
        }

        let _home = Restore("HOME", std::env::var_os("HOME"));
        let _profile = Restore("USERPROFILE", std::env::var_os("USERPROFILE"));
        std::env::set_var("HOME", "/c/Users/msys");
        std::env::set_var("USERPROFILE", "C:\\Users\\native");

        assert_eq!(
            home_dir(),
            PathBuf::from("C:\\Users\\native"),
            "an MSYS-style HOME must not win: native GCC cannot open that path"
        );
    }

    #[test]
    fn test_response_files_root_uses_fbuild_owned_tmp_dir() {
        let home = Path::new("/home/user");

        assert_eq!(
            response_files_root(home, false),
            home.join(crate::path::FBUILD_DIR_NAME)
                .join("prod")
                .join("tmp")
                .join("response-files")
        );
        assert_eq!(
            response_files_root(home, true),
            home.join(crate::path::FBUILD_DIR_NAME)
                .join("dev")
                .join("tmp")
                .join("response-files")
        );
    }

    #[test]
    fn test_cleanup_stale_response_files_removes_only_old_rsp_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let stale = tmp.path().join("fbuild_old.rsp");
        let fresh = tmp.path().join("fbuild_fresh.rsp");
        let other = tmp.path().join("notes.txt");

        std::fs::write(&stale, "old").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&fresh, "new").unwrap();
        std::fs::write(&other, "keep").unwrap();

        let removed =
            cleanup_stale_response_files(tmp.path(), Duration::from_millis(100), SystemTime::now())
                .unwrap();

        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(other.exists());
    }
}
