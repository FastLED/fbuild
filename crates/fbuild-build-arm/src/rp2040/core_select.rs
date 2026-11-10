//! Arduino core selection guard for RP2040 envs (FastLED/fbuild#1977).
//!
//! PlatformIO's `raspberrypi` platform builds the `pico` and
//! `nanorp2040connect` boards with the Arduino **Mbed OS RP2040** core
//! (`framework-arduino-mbed`) unless `board_build.core = earlephilhower`
//! selects Arduino-Pico. fbuild only implements Arduino-Pico, and the two
//! cores are not interchangeable, so an Mbed-core env is rejected up front
//! with an actionable error instead of being silently built against
//! Arduino-Pico (which previously failed later with a bare ENOENT).

use std::collections::HashMap;

/// Boards whose PlatformIO default Arduino core is Mbed OS RP2040.
const MBED_DEFAULT_BOARDS: &[&str] = &["pico", "nanorp2040connect"];

/// Fail fast when `env` selects the Arduino Mbed OS RP2040 core.
pub(crate) fn ensure_arduino_pico_core(env: &HashMap<String, String>) -> fbuild_core::Result<()> {
    let board = env.get("board").map(|b| b.trim()).unwrap_or_default();
    let core = env
        .get("board_build.core")
        .map(|c| c.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let mbed = match core.as_str() {
        "earlephilhower" => false,
        "" => MBED_DEFAULT_BOARDS.contains(&board),
        // Explicit `arduino`/`mbed` (or anything else) is not Arduino-Pico.
        _ => true,
    };
    if !mbed {
        return Ok(());
    }
    Err(fbuild_core::FbuildError::ConfigError(format!(
        "board `{board}` selects the Arduino Mbed OS RP2040 core \
         (framework-arduino-mbed), which fbuild does not support \
         (FastLED/fbuild#1977). fbuild builds RP2040 only with the \
         Arduino-Pico core. To use it, set `board_build.core = earlephilhower` \
         (and preferably `platform = https://github.com/maxgerhardt/platform-raspberrypi.git`, \
         `board = rpipico`); note Arduino-Pico is a different core than Mbed. \
         Otherwise build this env with PlatformIO."
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn stock_pico_is_mbed_and_rejected() {
        let err = ensure_arduino_pico_core(&env(&[
            ("platform", "raspberrypi"),
            ("board", "pico"),
            ("framework", "arduino"),
        ]))
        .unwrap_err()
        .to_string();
        assert!(err.contains("Mbed"), "{err}");
        assert!(err.contains("board_build.core = earlephilhower"), "{err}");
    }

    #[test]
    fn explicit_mbed_core_rejected() {
        assert!(
            ensure_arduino_pico_core(&env(&[
                ("board", "rpipico"),
                ("board_build.core", "arduino"),
            ]))
            .is_err()
        );
    }

    #[test]
    fn earlephilhower_core_accepted() {
        assert!(
            ensure_arduino_pico_core(&env(&[
                ("board", "pico"),
                ("board_build.core", "earlephilhower"),
            ]))
            .is_ok()
        );
    }

    #[test]
    fn arduino_pico_only_boards_accepted() {
        for board in ["rpipico", "rpipico2", "rpipico2w"] {
            assert!(
                ensure_arduino_pico_core(&env(&[("platform", "raspberrypi"), ("board", board)]))
                    .is_ok(),
                "{board}"
            );
        }
    }
}
