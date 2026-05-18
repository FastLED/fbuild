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
pub(super) fn parse_flags(flags_str: &str) -> Vec<String> {
    let mut result = Vec::new();

    for line in flags_str.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let chars = trimmed.chars();
        let mut current = String::new();
        let mut in_quotes = false;
        let mut quote_char = ' ';

        for c in chars {
            match c {
                '"' | '\'' if !in_quotes => {
                    in_quotes = true;
                    quote_char = c;
                    current.push(c);
                }
                c if in_quotes && c == quote_char => {
                    in_quotes = false;
                    current.push(c);
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
