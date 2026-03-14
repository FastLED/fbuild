//! Shell-style string splitting that respects quotes but treats backslashes as literal.
//!
//! Unlike POSIX `shlex`, this does not interpret `\` as an escape character —
//! critical for Windows paths like `C:\Users\...`.

/// Split a string into tokens on whitespace, respecting single and double quotes.
///
/// Quoted regions group text into a single token. Quote characters are stripped
/// from the output. Backslashes are treated as literal characters (no escaping).
///
/// ```
/// use fbuild_core::shell_split::split;
///
/// assert_eq!(split("-I/path -DFOO"), vec!["-I/path", "-DFOO"]);
/// assert_eq!(
///     split(r#"-I"C:\Program Files\SDK" -Ifoo"#),
///     vec![r"C:\Program Files\SDK".replace("C:", "-IC:"), "-Ifoo".to_string()]
/// );
/// ```
pub fn split(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for c in s.chars() {
        match in_quote {
            Some(q) if c == q => {
                in_quote = None;
            }
            Some(_) => {
                current.push(c);
            }
            None if c == '"' || c == '\'' => {
                in_quote = Some(c);
            }
            None if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => {
                current.push(c);
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic() {
        assert_eq!(split("a b c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn extra_whitespace() {
        assert_eq!(split("  a   b  "), vec!["a", "b"]);
    }

    #[test]
    fn double_quotes() {
        assert_eq!(
            split(r#"-I"C:\Program Files\SDK" -Ifoo"#),
            vec![r"-IC:\Program Files\SDK", "-Ifoo"]
        );
    }

    #[test]
    fn single_quotes() {
        assert_eq!(
            split("-I'/path with spaces/include' -Ibar"),
            vec!["-I/path with spaces/include", "-Ibar"]
        );
    }

    #[test]
    fn backslashes_literal() {
        let result = split(r"-IC:\Users\niteris\include -Ifoo");
        assert_eq!(result.len(), 2);
        assert!(result[0].contains(r"\Users\niteris"));
    }

    #[test]
    fn empty() {
        assert!(split("").is_empty());
        assert!(split("   ").is_empty());
    }

    #[test]
    fn iwithprefixbefore() {
        let result =
            split("-iwithprefixbefore freertos/include -iwithprefixbefore esp_system/include");
        assert_eq!(
            result,
            vec![
                "-iwithprefixbefore",
                "freertos/include",
                "-iwithprefixbefore",
                "esp_system/include"
            ]
        );
    }

    #[test]
    fn mixed_quotes() {
        assert_eq!(split(r#"a "b c" 'd e' f"#), vec!["a", "b c", "d e", "f"]);
    }

    #[test]
    fn newlines_and_tabs() {
        assert_eq!(split("a\tb\nc"), vec!["a", "b", "c"]);
    }
}
