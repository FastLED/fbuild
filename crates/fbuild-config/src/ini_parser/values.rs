//! Value-string helpers: inline-comment stripping, flag tokenization,
//! and multi-value list parsing.

/// Strip inline comments (` ; comment` or ` # comment`).
/// Be careful not to strip hash/semicolons that are part of values.
pub(super) fn strip_inline_comment(s: &str) -> String {
    // Only strip comments that are preceded by whitespace
    // This avoids stripping "#include" or URLs with "#"
    let bytes = s.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i - 1] == b' ' && (bytes[i] == b';' || bytes[i] == b'#') {
            return s[..i].trim().to_string();
        }
    }
    s.trim().to_string()
}

/// Parse build flags string into a list.
///
/// Handles:
/// - Space-separated flags: `-DFOO -DBAR`
/// - Multi-line: one flag per line
/// - `-D FLAG` → `-DFLAG` normalization
/// - Preserves arguments for `-include`, `-I`, `-L`, etc.
/// - Consumes the INI shell-quoting layer (FastLED/fbuild#947): quote
///   delimiters group and are stripped, and `\"` (outside single quotes)
///   is an escaped literal `"`. PlatformIO feeds `build_flags` through
///   Python `shlex`, so `-DNAME="\"Demo\""` must emerge here as the
///   single direct-exec argv element `-DNAME="Demo"`. Backslashes not
///   escaping a double quote stay literal (Windows paths) — the one
///   deliberate divergence from POSIX shlex.
pub(super) fn parse_flags(flags_str: &str) -> Vec<String> {
    let mut result = Vec::new();

    for line in flags_str.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut chars = trimmed.chars().peekable();
        let mut current = String::new();
        let mut in_quotes = false;
        let mut quote_char = ' ';

        while let Some(c) = chars.next() {
            match c {
                '\\' if chars.peek() == Some(&'"') && !(in_quotes && quote_char == '\'') => {
                    chars.next();
                    current.push('"');
                }
                '"' | '\'' if !in_quotes => {
                    in_quotes = true;
                    quote_char = c;
                }
                c if in_quotes && c == quote_char => {
                    in_quotes = false;
                }
                ' ' | '\t' if !in_quotes => {
                    if !current.is_empty() {
                        result.push(current.clone());
                        current.clear();
                    }
                }
                _ => {
                    current.push(c);
                }
            }
        }

        if !current.is_empty() {
            result.push(current);
        }
    }

    // Normalize `-D FLAG` → `-DFLAG`
    let mut normalized = Vec::new();
    let mut i = 0;
    while i < result.len() {
        if result[i] == "-D" && i + 1 < result.len() {
            normalized.push(format!("-D{}", result[i + 1]));
            i += 2;
        } else {
            normalized.push(result[i].clone());
            i += 1;
        }
    }

    normalized
}

/// Parse library dependencies from a multi-line or comma-separated string.
pub(super) fn parse_lib_deps(deps_str: &str) -> Vec<String> {
    let mut result = Vec::new();

    for line in deps_str.lines() {
        for dep in line.split(',') {
            let trimmed = dep.trim();
            if !trimmed.is_empty() {
                result.push(trimmed.to_string());
            }
        }
    }

    result
}

/// Parse a `PATH`-style list of paths from `PLATFORMIO_LIB_EXTRA_DIRS`.
pub(super) fn parse_path_list(paths_str: &str) -> Vec<String> {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let mut result = Vec::new();

    for line in paths_str.lines() {
        for path in line.split(separator) {
            let cleaned = strip_inline_comment(path);
            if !cleaned.is_empty() {
                result.push(cleaned);
            }
        }
    }

    result
}

/// Parse a generic multi-value option from a multi-line or comma-separated string.
pub(super) fn parse_list_values(value: &str) -> Vec<String> {
    let mut result = Vec::new();

    for line in value.lines() {
        for item in line.split(',') {
            let trimmed = item.trim();
            if !trimmed.is_empty() {
                result.push(trimmed.to_string());
            }
        }
    }

    result
}
