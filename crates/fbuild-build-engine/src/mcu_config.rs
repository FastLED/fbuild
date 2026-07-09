//! Shared MCU-configuration types used across every platform orchestrator.
//!
//! These types live at the engine layer (not inside any one platform module)
//! because they are referenced by many platforms. Keeping them here is what
//! lets the per-platform crates depend only on the engine, never on each other
//! — the ENGINE→PLATFORM = 0 invariant the compile-parallelism split relies on
//! (FastLED/fbuild#1008, Phase A0).

use serde::Deserialize;

/// A `defines` entry from a board/MCU JSON: either a bare macro name or a
/// `[name, value]` pair.
///
/// Historically defined in `esp32::mcu_config`; every other platform's
/// `mcu_config` (`esp8266`, `generic_arm`, `stm32`, `rp2040`, `apollo3`,
/// `nxplpc`, `nrf52`, `sam`, `silabs`, `renesas`, `ch32v`) parses the same
/// shape, so it now lives at the shared engine layer. `esp32::mcu_config`
/// re-exports it for source compatibility.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum DefineEntry {
    /// A bare `-D<NAME>` macro.
    Simple(String),
    /// A `-D<NAME>=<VALUE>` macro.
    KeyValue(String, String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_and_keyvalue_forms() {
        let simple: DefineEntry = serde_json::from_str(r#""FOO""#).unwrap();
        assert!(matches!(simple, DefineEntry::Simple(s) if s == "FOO"));
        let kv: DefineEntry = serde_json::from_str(r#"["BAR", "1"]"#).unwrap();
        assert!(matches!(kv, DefineEntry::KeyValue(k, v) if k == "BAR" && v == "1"));
    }
}
