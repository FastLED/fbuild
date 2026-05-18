//! Accessor / derivation methods on [`BoardConfig`].
//!
//! Includes emulator queries, ESP32 flash/PSRAM heuristics, platform
//! detection, preprocessor defines generation, and include-path resolution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::types::{BoardConfig, DebugToolMeta, Esp32QemuPsramConfig, EMULATOR_TOOL_NAMES};

impl BoardConfig {
    /// Returns emulator/simulator tools available for this board.
    ///
    /// Filters `debug_tools` to only include known software emulators
    /// (simavr, qemu, renode, ovpsim, verilator), excluding hardware debug probes.
    pub fn emulators(&self) -> HashMap<&str, &DebugToolMeta> {
        let Some(ref tools) = self.debug_tools else {
            return HashMap::new();
        };
        tools
            .iter()
            .filter(|(name, _)| EMULATOR_TOOL_NAMES.contains(&name.as_str()))
            .map(|(name, meta)| (name.as_str(), meta))
            .collect()
    }

    /// Check whether this board supports a specific emulator tool.
    pub fn has_emulator(&self, tool_name: &str) -> bool {
        self.debug_tools
            .as_ref()
            .is_some_and(|tools| tools.contains_key(tool_name))
            && EMULATOR_TOOL_NAMES.contains(&tool_name)
    }

    /// Resolve the effective ESP32 SDK memory profile used for variant headers/libs.
    ///
    /// This keeps the SDK `sdkconfig.h` and memory-profile libraries aligned
    /// with the repo's effective flash-mode policy. Boards that explicitly use
    /// OPI flash keep the `opi` flash-half because that represents a distinct
    /// bus type rather than an optional fast-read mode.
    pub fn effective_esp32_memory_type(&self, default_flash_mode: &str) -> Option<String> {
        if !self.mcu.starts_with("esp32") {
            return None;
        }

        let effective_flash_mode = self
            .flash_mode
            .as_deref()
            .unwrap_or(default_flash_mode)
            .to_ascii_lowercase();

        let (flash_half, psram_half) = if let Some(memory_type) = self.memory_type.as_deref() {
            if let Some((flash, psram)) = memory_type.split_once('_') {
                (
                    Some(flash.to_ascii_lowercase()),
                    Some(psram.to_ascii_lowercase()),
                )
            } else {
                (Some(memory_type.to_ascii_lowercase()), None)
            }
        } else {
            (None, None)
        };

        let resolved_flash = match flash_half.as_deref() {
            Some("opi") => "opi".to_string(),
            _ => effective_flash_mode,
        };
        let resolved_psram = psram_half
            .or_else(|| self.psram_type.as_deref().map(|s| s.to_ascii_lowercase()))
            .unwrap_or_else(|| "qspi".to_string());

        Some(format!("{}_{}", resolved_flash, resolved_psram))
    }

    pub fn qemu_esp32_psram_config(&self) -> Option<Esp32QemuPsramConfig> {
        let has_psram = self
            .extra_flags
            .as_deref()
            .is_some_and(|flags| extra_flags_contain_define(flags, "BOARD_HAS_PSRAM"))
            || self.psram_type.is_some();
        if !has_psram {
            return None;
        }

        let is_octal = self
            .psram_type
            .as_deref()
            .is_some_and(|psram| psram.eq_ignore_ascii_case("opi"))
            || self
                .memory_type
                .as_deref()
                .is_some_and(|memory| memory.ends_with("_opi"));
        let size_mib = infer_psram_size_mib(&self.name).unwrap_or(if is_octal { 8 } else { 2 });

        Some(Esp32QemuPsramConfig { size_mib, is_octal })
    }

    /// Detect the platform from the board JSON's platform field, or fall back to MCU heuristic.
    pub fn platform(&self) -> Option<fbuild_core::Platform> {
        // Prefer explicit platform from board JSON (distinguishes AtmelMegaAvr from AtmelAvr)
        if let Some(ref p) = self.platform_str {
            if let Some(platform) = fbuild_core::Platform::from_platform_str(p) {
                return Some(platform);
            }
        }
        let mcu = self.mcu.to_lowercase();
        if mcu.starts_with("atmega") || mcu.starts_with("attiny") || mcu.starts_with("at90") {
            Some(fbuild_core::Platform::AtmelAvr)
        } else if mcu.starts_with("esp32") {
            Some(fbuild_core::Platform::Espressif32)
        } else if mcu.starts_with("esp8266") || mcu.starts_with("esp8285") {
            Some(fbuild_core::Platform::Espressif8266)
        } else if mcu.starts_with("imxrt") || mcu.starts_with("mk") {
            Some(fbuild_core::Platform::Teensy)
        } else if mcu.starts_with("rp2040") || mcu.starts_with("rp2350") {
            Some(fbuild_core::Platform::RaspberryPi)
        } else if mcu.starts_with("stm32") {
            Some(fbuild_core::Platform::Ststm32)
        } else if mcu.starts_with("nrf52") {
            Some(fbuild_core::Platform::NordicNrf52)
        } else if mcu.starts_with("at91sam") || mcu.starts_with("sam") {
            Some(fbuild_core::Platform::AtmelSam)
        } else if mcu.starts_with("ra4") || mcu.starts_with("ra6") {
            Some(fbuild_core::Platform::RenesasRa)
        } else if mcu.starts_with("ch32") {
            Some(fbuild_core::Platform::Ch32v)
        } else if mcu.starts_with("apollo3") || mcu.starts_with("ama3b") {
            Some(fbuild_core::Platform::Apollo3)
        } else {
            None
        }
    }

    /// Generate preprocessor defines for this board.
    ///
    /// Returns defines like: PLATFORMIO, F_CPU, ARDUINO, `ARDUINO_<BOARD>`, `ARDUINO_ARCH_<ARCH>`
    pub fn get_defines(&self) -> HashMap<String, String> {
        let mut defines = HashMap::new();

        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        defines.insert("F_CPU".to_string(), self.f_cpu.clone());

        // Default Arduino version. Platform-specific overrides (e.g. Teensy=10819)
        // are in MCU config JSON defines, merged by the orchestrator after this.
        defines.insert("ARDUINO".to_string(), "10808".to_string());

        defines.insert(
            format!("ARDUINO_{}", self.board.to_uppercase()),
            "1".to_string(),
        );
        // ARDUINO_BOARD and ARDUINO_VARIANT as quoted string defines.
        // Use \" escapes so GCC response files on Windows preserve the quotes
        // (bare " is treated as a word delimiter by GCC's response file parser).
        defines.insert(
            "ARDUINO_BOARD".to_string(),
            format!("\\\"{}\\\"", self.board),
        );
        defines.insert(
            "ARDUINO_VARIANT".to_string(),
            format!("\\\"{}\\\"", self.variant),
        );

        // Architecture define
        let arch = self.arch_define();
        if !arch.is_empty() {
            defines.insert(format!("ARDUINO_ARCH_{}", arch), "1".to_string());
        }

        // MCU-specific define for AVR
        let mcu_upper = self.mcu.to_uppercase();
        if mcu_upper.starts_with("ATMEGA") || mcu_upper.starts_with("ATTINY") {
            defines.insert(format!("__AVR_{}__", mcu_upper), "1".to_string());
        }

        // Teensy __MCU__ define (MCU detection, not a versioned constant)
        if matches!(self.platform(), Some(fbuild_core::Platform::Teensy))
            && mcu_upper.starts_with("IMXRT")
        {
            defines.insert(format!("__{}__", mcu_upper), "1".to_string());
        }

        // USB VID/PID defines for USB-native boards (Leonardo, Micro, etc.)
        if let Some(ref vid) = self.vid {
            defines.insert("USB_VID".to_string(), vid.clone());
        }
        if let Some(ref pid) = self.pid {
            defines.insert("USB_PID".to_string(), pid.clone());
        }

        // Extra flags
        if let Some(ref flags) = self.extra_flags {
            for flag in fbuild_core::shell_split::split(flags) {
                if let Some(define) = flag.strip_prefix("-D") {
                    if let Some(eq_pos) = define.find('=') {
                        defines.insert(
                            define[..eq_pos].to_string(),
                            define[eq_pos + 1..].to_string(),
                        );
                    } else {
                        defines.insert(define.to_string(), "1".to_string());
                    }
                }
            }
        }

        defines
    }

    /// Get include paths relative to a framework root directory.
    ///
    /// Returns: `[cores/<core>, variants/<variant>]`
    pub fn get_include_paths(&self, framework_root: &Path) -> Vec<PathBuf> {
        vec![
            framework_root.join("cores").join(&self.core),
            framework_root.join("variants").join(&self.variant),
        ]
    }

    fn arch_define(&self) -> String {
        match self.platform() {
            Some(fbuild_core::Platform::AtmelAvr) => "AVR".to_string(),
            Some(fbuild_core::Platform::AtmelMegaAvr) => "MEGAAVR".to_string(),
            Some(fbuild_core::Platform::Espressif32) => "ESP32".to_string(),
            Some(fbuild_core::Platform::Espressif8266) => "ESP8266".to_string(),
            Some(fbuild_core::Platform::NordicNrf52) => "NRF52".to_string(),
            Some(fbuild_core::Platform::RaspberryPi) => "RP2040".to_string(),
            Some(fbuild_core::Platform::RenesasRa) => "RENESAS".to_string(),
            Some(fbuild_core::Platform::SiliconLabs) => "SILABS".to_string(),
            Some(fbuild_core::Platform::Ststm32) => "STM32".to_string(),
            Some(fbuild_core::Platform::AtmelSam) => "SAM".to_string(),
            Some(fbuild_core::Platform::Teensy) => "TEENSY".to_string(),
            Some(fbuild_core::Platform::Ch32v) => "CH32V".to_string(),
            Some(fbuild_core::Platform::Apollo3) => "APOLLO3".to_string(),
            Some(fbuild_core::Platform::Wasm) | None => String::new(),
        }
    }
}

fn extra_flags_contain_define(extra_flags: &str, define: &str) -> bool {
    extra_flags.split_whitespace().any(|flag| {
        let Some(raw) = flag.strip_prefix("-D") else {
            return false;
        };
        raw.split_once('=').map_or(raw, |(name, _)| name) == define
    })
}

fn infer_psram_size_mib(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();

    for size in [32_u32, 16, 8, 4, 2] {
        if upper.contains(&format!("{size} MB PSRAM")) {
            return Some(size);
        }
    }

    for (marker, size) in [
        ("R32", 32_u32),
        ("R16", 16),
        ("R8", 8),
        ("R4", 4),
        ("R2", 2),
    ] {
        if upper.contains(marker) {
            return Some(size);
        }
    }

    None
}
