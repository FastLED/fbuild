//! Parsers for the flag files shipped with the ESP-IDF SDK package.

use std::path::{Path, PathBuf};

/// Parse include flags from the `flags/includes` file.
///
/// Uses `fbuild_core::shell_split::split` to tokenize (handles quoted paths,
/// safe on Windows). Iterates with an index, consuming flag+path pairs.
/// Split a defines string into individual `-D` flags, preserving escaped quotes.
///
/// The `flags/defines` file contains flags like:
/// ```text
/// -DFOO=1 -DBAR=\"baz.h\" -DQUX
/// ```
/// The `\"` sequences must be preserved because GCC needs the quotes in
/// define values (e.g., `MBEDTLS_CONFIG_FILE` expands to `"mbedtls/esp_config.h"`).
///
/// Unlike `shell_split`, this splits on whitespace boundaries that precede `-D`
/// and keeps the raw content of each flag intact.
pub(crate) fn split_defines(content: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    // Split on " -D" boundaries (preserving the -D prefix)
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return tokens;
    }
    // Find each -D token boundary
    let mut start = 0;
    let bytes = trimmed.as_bytes();
    let len = bytes.len();
    let mut i = 1; // skip first char
    while i < len {
        // A new -D token starts when we see whitespace followed by -D
        if bytes[i] == b'-'
            && i + 1 < len
            && bytes[i + 1] == b'D'
            && i > 0
            && bytes[i - 1].is_ascii_whitespace()
        {
            let token = trimmed[start..i].trim();
            if !token.is_empty() {
                tokens.push(token.to_string());
            }
            start = i;
        }
        i += 1;
    }
    // Last token
    let token = trimmed[start..].trim();
    if !token.is_empty() {
        tokens.push(token.to_string());
    }
    tokens
}

/// Handles two flag formats:
/// - `-iwithprefixbefore relative/path` (new 3.3.7+, resolved against include_base)
/// - `-I/absolute/path` or `-Irelative/path` (legacy 3.1.x)
pub(crate) fn parse_include_flags(content: &str, include_base: &Path, root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let parts = fbuild_core::shell_split::split(content);
    let mut i = 0;
    while i < parts.len() {
        if parts[i] == "-iwithprefixbefore" {
            if i + 1 < parts.len() {
                let resolved = include_base.join(&parts[i + 1]);
                if resolved.exists() {
                    dirs.push(resolved);
                }
                i += 2;
            } else {
                i += 1;
            }
        } else if let Some(path_str) = parts[i].strip_prefix("-I") {
            if !path_str.is_empty() {
                let p = if Path::new(path_str).is_absolute() {
                    PathBuf::from(path_str)
                } else {
                    root.join(path_str)
                };
                if p.exists() {
                    dirs.push(p);
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    dirs
}

/// Extract a version string from a framework URL.
///
/// E.g., `".../download/3.3.7/esp32-core-3.3.7.tar.xz"` → `"3.3.7"`
pub(crate) fn extract_framework_version(url: &str) -> String {
    // Look for a path segment that is purely a version number (digits + dots)
    for segment in url.rsplit('/') {
        let s = segment
            .trim_end_matches(".tar.xz")
            .trim_end_matches(".tar.gz")
            .trim_end_matches(".zip");
        if s.chars().all(|c| c.is_ascii_digit() || c == '.') && s.contains('.') && !s.is_empty() {
            return s.to_string();
        }
    }
    // Fallback: hash
    crate::cache::hash_url(url)
}
