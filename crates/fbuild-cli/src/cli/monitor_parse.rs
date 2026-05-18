//! Small parsers shared by the `--monitor "..."` flag, `--jobs`, and
//! anything else that needs shell-style tokenization.

/// Parsed monitor flags extracted from a `--monitor="..."` string.
#[derive(Default)]
pub struct ParsedMonitorFlags {
    pub timeout: Option<f64>,
    pub halt_on_error: Option<String>,
    pub halt_on_success: Option<String>,
    pub expect: Option<String>,
}

/// Validate that jobs count is >= 1 (matches Python behavior).
pub fn parse_jobs(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|e| format!("{e}"))?;
    if n == 0 {
        return Err("jobs must be >= 1".to_string());
    }
    Ok(n)
}

/// Parse monitor flags from a string like `--timeout 60 --halt-on-success "TEST PASSED"`.
pub fn parse_monitor_flags(s: &str) -> ParsedMonitorFlags {
    let mut result = ParsedMonitorFlags::default();
    let tokens = shell_tokenize(s);
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "--timeout" | "-t" => {
                if let Some(val) = tokens.get(i + 1) {
                    result.timeout = val.parse().ok();
                    i += 1;
                }
            }
            "--halt-on-error" => {
                if let Some(val) = tokens.get(i + 1) {
                    result.halt_on_error = Some(val.clone());
                    i += 1;
                }
            }
            "--halt-on-success" => {
                if let Some(val) = tokens.get(i + 1) {
                    result.halt_on_success = Some(val.clone());
                    i += 1;
                }
            }
            "--expect" => {
                if let Some(val) = tokens.get(i + 1) {
                    result.expect = Some(val.clone());
                    i += 1;
                }
            }
            other => {
                // Handle --key=value form
                if let Some(rest) = other.strip_prefix("--timeout=") {
                    result.timeout = rest.parse().ok();
                } else if let Some(rest) = other.strip_prefix("--halt-on-error=") {
                    result.halt_on_error = Some(rest.to_string());
                } else if let Some(rest) = other.strip_prefix("--halt-on-success=") {
                    result.halt_on_success = Some(rest.to_string());
                } else if let Some(rest) = other.strip_prefix("--expect=") {
                    result.expect = Some(rest.to_string());
                }
            }
        }
        i += 1;
    }
    result
}

/// Simple shell-style tokenizer that handles quoted strings.
pub fn shell_tokenize(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape_next = false;

    for ch in s.chars() {
        if escape_next {
            current.push(ch);
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if !in_single_quote => {
                escape_next = true;
            }
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}
