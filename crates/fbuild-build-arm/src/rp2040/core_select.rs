//! Arduino core selection guard for RP2040 envs (FastLED/fbuild#1977).
//!
//! PlatformIO's `raspberrypi` platform builds the `pico` and
//! `nanorp2040connect` boards with the Arduino **Mbed OS RP2040** core
//! (`framework-arduino-mbed`) unless `board_build.core = earlephilhower`
//! selects Arduino-Pico. The two cores are not interchangeable: dispatch to
//! their distinct build recipes, and prevent the Arduino-Pico-only package
//! resolver from silently provisioning the wrong SDK for a Mbed environment.

use std::collections::HashMap;

/// Boards whose PlatformIO default Arduino core is Mbed OS RP2040.
const MBED_DEFAULT_BOARDS: &[&str] = &["pico", "nanorp2040connect"];

/// Framework family selected by an RP2040 environment.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ArduinoCore {
    Pico,
    Mbed,
}

pub(crate) fn selected_core(env: &HashMap<String, String>) -> fbuild_core::Result<ArduinoCore> {
    let board = env.get("board").map(|b| b.trim()).unwrap_or_default();
    let core = env
        .get("board_build.core")
        .map(|c| c.trim().to_ascii_lowercase())
        .unwrap_or_default();
    match core.as_str() {
        "earlephilhower" => Ok(ArduinoCore::Pico),
        "arduino" | "mbed" => Ok(ArduinoCore::Mbed),
        "" if MBED_DEFAULT_BOARDS.contains(&board) => Ok(ArduinoCore::Mbed),
        "" => Ok(ArduinoCore::Pico),
        _ => Err(fbuild_core::FbuildError::ConfigError(format!(
            "unsupported RP2040 Arduino core `{core}`"
        ))),
    }
}

/// Guard callers of the Arduino-Pico-only package resolver.
pub(crate) fn ensure_arduino_pico_core(env: &HashMap<String, String>) -> fbuild_core::Result<()> {
    let board = env.get("board").map(|b| b.trim()).unwrap_or_default();
    if selected_core(env)? == ArduinoCore::Pico {
        return Ok(());
    }
    Err(fbuild_core::FbuildError::ConfigError(format!(
        "board `{board}` selects the Arduino Mbed OS RP2040 core \
         (framework-arduino-mbed), not Arduino-Pico. Use the Mbed recipe \
         instead of the Arduino-Pico package resolver. To explicitly select \
         Arduino-Pico, set `board_build.core = earlephilhower`; the two cores \
         are not interchangeable (FastLED/fbuild#1977)."
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
    fn stock_pico_selects_mbed_and_explicit_pico_override_is_preserved() {
        assert_eq!(
            selected_core(&env(&[("board", "pico")])).unwrap(),
            ArduinoCore::Mbed
        );
        assert_eq!(
            selected_core(&env(&[("board", "nanorp2040connect")])).unwrap(),
            ArduinoCore::Mbed
        );
        assert_eq!(
            selected_core(&env(&[("board", "rpipico")])).unwrap(),
            ArduinoCore::Pico
        );
        assert_eq!(
            selected_core(&env(&[
                ("board", "pico"),
                ("board_build.core", "earlephilhower")
            ]))
            .unwrap(),
            ArduinoCore::Pico
        );
        assert!(
            selected_core(&env(&[("board", "pico"), ("board_build.core", "unknown")])).is_err()
        );
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
