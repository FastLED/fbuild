//! GCC response file utilities shared across all fbuild crates.
//!
//! Handles writing `@file` response files for GCC/G++ on Windows where
//! command-line length limits (32KB CreateProcess) and MSYS2 path translation
//! issues require special handling.

use crate::Result;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
/// Returns the path to the response file. Uses an atomic counter for
/// thread-safe unique filenames during parallel compilation.
///
/// Flags containing `\"` (escaped quotes in define values) are wrapped in
/// single quotes with `\"` converted to plain `"` — GCC's response file
/// parser always preserves literal `"` inside single-quoted arguments.
pub fn write_response_file(flags: &[String], temp_dir: &Path, prefix: &str) -> Result<PathBuf> {
    static RSP_COUNTER: AtomicU64 = AtomicU64::new(0);

    std::fs::create_dir_all(temp_dir).map_err(|e| {
        crate::FbuildError::BuildFailed(format!(
            "failed to create temp dir {}: {}",
            temp_dir.display(),
            e
        ))
    })?;

    let counter = RSP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = temp_dir.join(format!(
        "fbuild_{}_{}_{}.rsp",
        prefix,
        std::process::id(),
        counter
    ));

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
}
