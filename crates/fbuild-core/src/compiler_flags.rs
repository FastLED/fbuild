//! Compiler flag escaping utilities.
//!
//! Provides platform-correct escaping for GCC `-D` define flags. Two code paths
//! exist for passing flags to GCC:
//!
//! 1. **Direct exec** (`Command::new()` → argv) — used on Linux/macOS.
//!    Backslash-escaped quotes (`\"`) become literal `\` + `"` characters, which
//!    GCC rejects as "stray '\\' in program". Use [`prepare_flags_for_exec`] to
//!    strip the escaping.
//!
//! 2. **Response files** (`@file` syntax) — used on Windows.
//!    The response file writer handles escaping separately (single-quote wrapping).
//!
//! All code that passes compiler flags containing define values to `run_command`
//! on non-Windows paths **must** call [`prepare_flags_for_exec`].

/// Prepare compiler flags for direct execution (no response file).
///
/// Strips backslash-escaped quotes (`\"`) so GCC receives the intended define
/// value. When invoked via `Command::new()`, each argv element is passed
/// literally — no shell interpretation — so `\"` is a literal backslash+quote
/// that GCC cannot parse.
///
/// # Example
///
/// ```
/// use fbuild_core::compiler_flags::prepare_flags_for_exec;
///
/// let flags = vec![
///     r#"-DARDUINO_BOARD=\"ESP32_DEV\""#.to_string(),
///     "-DPLATFORMIO".to_string(),
///     "-Wall".to_string(),
/// ];
/// let result = prepare_flags_for_exec(flags);
/// assert_eq!(result[0], r#"-DARDUINO_BOARD="ESP32_DEV""#);
/// assert_eq!(result[1], "-DPLATFORMIO");
/// assert_eq!(result[2], "-Wall");
/// ```
pub fn prepare_flags_for_exec(flags: Vec<String>) -> Vec<String> {
    flags
        .into_iter()
        .map(|f| {
            if f.contains("\\\"") {
                f.replace("\\\"", "\"")
            } else {
                f
            }
        })
        .collect()
}

/// Extract GCC definitions from tokenized flags, accepting both `-DNAME`
/// and `-D NAME`. Later definitions override earlier ones. Replacement text
/// (including quotes and additional `=` characters) is preserved verbatim.
pub fn parse_define_flags(flags: &[String]) -> std::collections::HashMap<String, String> {
    let mut defines = std::collections::HashMap::new();
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        let Some(raw) = flag.strip_prefix("-D") else {
            continue;
        };
        let definition = if raw.is_empty() {
            flags.next().map(String::as_str).unwrap_or_default()
        } else {
            raw
        };
        let (name, value) = definition.split_once('=').unwrap_or((definition, "1"));
        defines.insert(name.to_string(), value.to_string());
    }
    defines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separated_and_joined_define_flags_preserve_replacements() {
        let flags = [
            "-D",
            "BOARD",
            "-DVALUE=old",
            "-D",
            "VALUE=new=tail",
            "-DUSB_NAME=\"My Board\"",
            "-I/include",
            "-DEMPTY=",
        ]
        .map(str::to_string);
        let defines = parse_define_flags(&flags);
        assert_eq!(defines.get("BOARD").map(String::as_str), Some("1"));
        assert_eq!(defines.get("VALUE").map(String::as_str), Some("new=tail"));
        assert_eq!(
            defines.get("USB_NAME").map(String::as_str),
            Some("\"My Board\"")
        );
        assert_eq!(defines.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(defines.len(), 4);
    }

    #[test]
    fn strips_escaped_quotes_in_defines() {
        let flags = vec![
            r#"-DARDUINO_BOARD=\"ESP32_DEV\""#.to_string(),
            r#"-DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\""#.to_string(),
            r#"-DIDF_VER=\"v5.3.2\""#.to_string(),
        ];
        let result = prepare_flags_for_exec(flags);
        assert_eq!(result[0], r#"-DARDUINO_BOARD="ESP32_DEV""#);
        assert_eq!(result[1], r#"-DMBEDTLS_CONFIG_FILE="mbedtls/esp_config.h""#);
        assert_eq!(result[2], r#"-DIDF_VER="v5.3.2""#);
    }

    #[test]
    fn preserves_normal_flags() {
        let flags = vec![
            "-DPLATFORMIO".to_string(),
            "-DF_CPU=16000000L".to_string(),
            "-I/usr/include".to_string(),
            "-c".to_string(),
            "-Wall".to_string(),
        ];
        let result = prepare_flags_for_exec(flags.clone());
        assert_eq!(result, flags);
    }

    #[test]
    fn empty_input() {
        assert!(prepare_flags_for_exec(Vec::new()).is_empty());
    }
}
