//! Per-board deploy method enumeration + fail-fast validation.
//!
//! FastLED/fbuild#692. PlatformIO board JSON carries
//! `upload.protocol` (the default) and `upload.protocols` (the array
//! of every supported method) — fbuild reads it and rejects requests
//! that name a method not in the supported list, with a structured
//! error that names the board, the requested method, and the
//! alternatives.
//!
//! Today's failure mode is "30-180 s timeout" or "backend-specific
//! error that doesn't say `this board doesn't do OTA`." Pre-deploy
//! validation collapses both into one clear error before any
//! backend is invoked.

use fbuild_core::{FbuildError, Result};

/// Validate that `requested` is in the board's `supported` list.
///
/// - `Ok(())` — supported, deploy proceeds.
/// - `Err(FbuildError::UnsupportedDeployMethod { board, requested,
///   supported })` — fail fast. The `supported` field is a
///   comma-separated string of alternatives so the error's `Display`
///   spells them out.
///
/// **Case-insensitive matching** — board JSON sometimes carries
/// `"CMSIS-DAP"` while users pass `--use-cmsis-dap`. The check
/// normalizes both sides to lowercase before comparing.
///
/// **Empty `supported`** is treated as a config bug (every board
/// must list at least one protocol) and surfaces as an unsupported
/// error with `supported: "(none — board JSON is missing
/// upload.protocols)"`. The caller can then fix the board JSON
/// instead of debugging a silent fallback.
pub fn validate_deploy_method(
    board_name: &str,
    requested: &str,
    supported: &[String],
) -> Result<()> {
    let requested_norm = requested.trim().to_ascii_lowercase();
    if supported.is_empty() {
        return Err(FbuildError::UnsupportedDeployMethod {
            board: board_name.to_string(),
            requested: requested.to_string(),
            supported: "(none — board JSON is missing upload.protocols)".to_string(),
        });
    }
    for s in supported {
        if s.trim().to_ascii_lowercase() == requested_norm {
            return Ok(());
        }
    }
    let supported_csv = supported
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    Err(FbuildError::UnsupportedDeployMethod {
        board: board_name.to_string(),
        requested: requested.to_string(),
        supported: supported_csv,
    })
}

/// Pick the default deploy method when the user did not pass an
/// explicit `--use-XXXX` flag.
///
/// Returns the first entry in `supported` — board JSON convention is
/// to list the canonical method first (`upload.protocol` in
/// PlatformIO terms; PlatformIO emits the array with that protocol
/// at index 0).
///
/// Returns `None` if `supported` is empty (config bug — caller
/// should surface it).
#[must_use]
pub fn default_deploy_method(supported: &[String]) -> Option<&str> {
    supported.first().map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn supported_method_is_accepted() {
        validate_deploy_method("lpc845brk", "cmsis-dap", &s(&["cmsis-dap", "mbed"])).unwrap();
    }

    #[test]
    fn unsupported_method_is_rejected_with_structured_fields() {
        let err =
            validate_deploy_method("lpc845brk", "ota", &s(&["cmsis-dap", "mbed"])).unwrap_err();
        match err {
            FbuildError::UnsupportedDeployMethod {
                board,
                requested,
                supported,
            } => {
                assert_eq!(board, "lpc845brk");
                assert_eq!(requested, "ota");
                assert!(supported.contains("cmsis-dap"));
                assert!(supported.contains("mbed"));
            }
            other => panic!("expected UnsupportedDeployMethod, got {other:?}"),
        }
    }

    #[test]
    fn case_insensitive_match_accepts_capitalized_board_json() {
        // Board JSON has "CMSIS-DAP" but the user passes
        // "--use-cmsis-dap" → lowercased "cmsis-dap". MUST accept.
        validate_deploy_method("lpc845brk", "cmsis-dap", &s(&["CMSIS-DAP", "MBED"])).unwrap();
    }

    /// LPC845-BRK row: CMSIS-DAP + mbed, NO OTA. Issue's worked
    /// example.
    #[test]
    fn lpc845brk_supports_cmsis_dap_not_ota() {
        let supported = s(&["cmsis-dap", "mbed"]);
        validate_deploy_method("lpc845brk", "cmsis-dap", &supported).unwrap();
        assert!(validate_deploy_method("lpc845brk", "ota", &supported).is_err());
    }

    /// ESP32 row: OTA + esptool + esp-prog. All accepted.
    #[test]
    fn esp32_supports_ota_esptool_esp_prog() {
        let supported = s(&["esptool", "esp-prog", "ota"]);
        for method in &["esptool", "esp-prog", "ota"] {
            validate_deploy_method("esp32dev", method, &supported).unwrap();
        }
    }

    /// Teensy row: HID only, no OTA.
    #[test]
    fn teensy_rejects_ota() {
        let supported = s(&["teensy-cli", "teensy-gui"]);
        validate_deploy_method("teensy40", "teensy-cli", &supported).unwrap();
        let err = validate_deploy_method("teensy40", "ota", &supported).unwrap_err();
        match err {
            FbuildError::UnsupportedDeployMethod { supported, .. } => {
                assert!(supported.contains("teensy-cli"));
            }
            other => panic!("expected UnsupportedDeployMethod, got {other:?}"),
        }
    }

    /// RP2040 row: BOOTSEL → MSC drag-drop OR picotool DFU. No OTA.
    #[test]
    fn rp2040_rejects_ota() {
        let supported = s(&["picotool", "cmsis-dap", "mbed"]);
        validate_deploy_method("pico", "picotool", &supported).unwrap();
        assert!(validate_deploy_method("pico", "ota", &supported).is_err());
    }

    #[test]
    fn empty_supported_surfaces_config_bug() {
        let err = validate_deploy_method("broken_board", "ota", &[]).unwrap_err();
        match err {
            FbuildError::UnsupportedDeployMethod {
                supported, board, ..
            } => {
                assert_eq!(board, "broken_board");
                assert!(supported.contains("missing upload.protocols"));
            }
            other => panic!("expected UnsupportedDeployMethod, got {other:?}"),
        }
    }

    #[test]
    fn default_deploy_method_returns_first_entry() {
        let supported = s(&["cmsis-dap", "mbed"]);
        assert_eq!(default_deploy_method(&supported), Some("cmsis-dap"));
    }

    #[test]
    fn default_deploy_method_returns_none_for_empty() {
        assert_eq!(default_deploy_method(&[]), None);
    }

    #[test]
    fn requested_trim_and_lowercase() {
        let supported = s(&["esptool"]);
        // Leading whitespace from a sloppy CLI parse path → still
        // accepted.
        validate_deploy_method("esp32dev", "  esptool  ", &supported).unwrap();
        // Uppercase → still accepted.
        validate_deploy_method("esp32dev", "ESPTOOL", &supported).unwrap();
    }
}
