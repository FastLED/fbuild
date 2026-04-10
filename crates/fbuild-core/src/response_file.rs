//! GCC response file utilities shared across all fbuild crates.
//!
//! Handles writing `@file` response files for GCC/G++ on Windows where
//! command-line length limits (32KB CreateProcess) and MSYS2 path translation
//! issues require special handling.

use crate::Result;
use std::path::{Path, PathBuf};

/// Get the platform-appropriate temp directory for response files.
///
/// On MSYS2/Git Bash, `std::env::temp_dir()` returns `/tmp/` which native
/// Windows GCC treats as `C:\tmp\`. Use `LOCALAPPDATA\Temp` instead.
pub fn windows_temp_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var("LOCALAPPDATA")
            .map(|la| PathBuf::from(la).join("Temp"))
            .unwrap_or_else(|_| std::env::temp_dir())
    } else {
        std::env::temp_dir()
    }
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

/// Write flags to a temporary GCC response file (`@file` syntax).
///
/// Returns the path to the response file.
///
/// Flags containing `\"` (escaped quotes in define values) are wrapped in
/// single quotes with `\"` converted to plain `"` — GCC's response file
/// parser always preserves literal `"` inside single-quoted arguments.
pub fn write_response_file(flags: &[String], temp_dir: &Path, prefix: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(temp_dir).map_err(|e| {
        crate::FbuildError::BuildFailed(format!(
            "failed to create temp dir {}: {}",
            temp_dir.display(),
            e
        ))
    })?;

    // GCC treats backslashes in response files as escape characters (\n = newline,
    // \f = formfeed, etc.). Convert to forward slashes for Windows path compatibility,
    // but preserve \" sequences which are intentional escape sequences (e.g., in
    // -DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\").
    //
    // Flags containing \" (escaped quotes in define values like -DARDUINO_BOARD=\"...\")
    // must be wrapped in single quotes with the \" converted to plain " — GCC's
    // response file parser treats \" inconsistently across platforms, but single-quoted
    // arguments always preserve literal " characters.
    let content = flags
        .iter()
        .map(|f| {
            let fwd = replace_path_backslashes(f);
            if fwd.contains("\\\"") {
                let unescaped = fwd.replace("\\\"", "\"");
                format!("'{}'", unescaped)
            } else if fwd.contains(' ') {
                format!("\"{}\"", fwd)
            } else {
                fwd
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

    #[test]
    fn test_write_response_file_reuses_same_path_for_same_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let flags = vec!["-O2".to_string(), "-c".to_string(), "src.cpp".to_string()];

        let first = write_response_file(&flags, tmp.path(), "stable").unwrap();
        let second = write_response_file(&flags, tmp.path(), "stable").unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn test_write_response_file_changes_path_when_content_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let first = write_response_file(&["-O2".to_string()], tmp.path(), "stable").unwrap();
        let second = write_response_file(&["-O3".to_string()], tmp.path(), "stable").unwrap();

        assert_ne!(first, second);
    }
}
