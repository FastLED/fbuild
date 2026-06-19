//! Board configuration data types.
//!
//! Defines the [`BoardConfig`] struct loaded from boards.txt and built-in
//! defaults, along with supporting metadata types ([`DebugToolMeta`],
//! [`Esp32QemuPsramConfig`]) and module-private constants.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Esp32QemuPsramConfig {
    pub size_mib: u32,
    pub is_octal: bool,
}

/// Metadata for a single debug tool entry from the board JSON `debug.tools` section.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DebugToolMeta {
    /// Whether the tool is built into the board (no external hardware needed).
    #[serde(default)]
    pub onboard: bool,
    /// Whether this is the board's default debug tool.
    #[serde(default)]
    pub default: bool,
}

/// Known emulator/simulator tool names that can run firmware without hardware.
pub(super) const EMULATOR_TOOL_NAMES: &[&str] =
    &["simavr", "qemu", "renode", "ovpsim", "verilator"];

/// Board configuration loaded from boards.txt or built-in defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardConfig {
    pub name: String,
    pub mcu: String,
    pub f_cpu: String,
    pub board: String,
    pub core: String,
    pub variant: String,
    /// Variant header override for frameworks that use `#include VARIANT_H`
    pub variant_h: Option<String>,
    /// ESP32 chip-variant SDK selector (Arduino `build.chip_variant`).
    ///
    /// Names the `esp32-arduino-libs/<chip_variant>` directory whose prebuilt
    /// libraries, linker scripts, and bootloader are linked against a specific
    /// ROM revision. When `None`, the SDK directory falls back to `mcu`.
    /// ESP32-P4 needs this: `esp32p4_es` targets chip rev v0.x–v1.x (eco0–eco2),
    /// while `esp32p4` targets rev v3.x (eco5+). Linking the wrong one boots
    /// into an illegal-instruction panic at the bootloader entry point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chip_variant: Option<String>,
    /// USB vendor ID (optional)
    pub vid: Option<String>,
    /// USB product ID (optional)
    pub pid: Option<String>,
    /// Extra build flags from board definition
    pub extra_flags: Option<String>,
    /// Upload protocol (e.g. "arduino", "esptool", "teensy-gui")
    pub upload_protocol: Option<String>,
    /// Upload speed
    pub upload_speed: Option<String>,
    /// PlatformIO serial monitor filters.
    ///
    /// ESP32-family boards default to `default, esp32_exception_decoder` when
    /// unset. An explicit empty list in project config suppresses that default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_filters: Option<Vec<String>>,
    /// Maximum flash size in bytes
    pub max_flash: Option<u64>,
    /// Maximum RAM size in bytes
    pub max_ram: Option<u64>,
    /// Flash mode (e.g. "dio", "qio") — ESP32 boards
    pub flash_mode: Option<String>,
    /// Memory profile (e.g. "qio_qspi", "qio_opi") - ESP32 boards
    pub memory_type: Option<String>,
    /// PSRAM type (e.g. "qspi", "opi") - ESP32 boards
    pub psram_type: Option<String>,
    /// Flash frequency (e.g. "80000000L") — ESP32 boards
    pub f_flash: Option<String>,
    /// Image flash frequency override (e.g. "48000000L") — used by esptool when
    /// the board's actual SPI clock (`f_flash`) doesn't match a valid esptool frequency.
    /// PlatformIO calls this `build.f_image`. When present, this takes priority over
    /// `f_flash` for esptool's `--flash-freq` argument.
    pub f_image: Option<String>,
    /// Partition table file (e.g. "default_8MB.csv") — ESP32 boards
    pub partitions: Option<String>,
    /// Linker script (e.g. "esp32s3_out.ld")
    pub ldscript: Option<String>,
    /// OpenOCD target script from board metadata, when provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openocd_target: Option<String>,
    /// Platform string from board JSON (e.g. "atmelmegaavr", "atmelavr")
    pub platform_str: Option<String>,
    /// Bare CMSIS-DSP math library name to auto-link, without the leading `lib`
    /// and `.a` (e.g. `arm_cortexM4lf_math`, `arm_cortexM7lfsp_math`).
    ///
    /// Populated from board JSON `build.cmsis_dsp_lib`. Mirrors the behaviour
    /// of PlatformIO+Teensyduino's SCons builder, which auto-appends the right
    /// CMSIS-DSP archive to the link command based on MCU so that Teensy
    /// `Audio.h` FFT classes (and anything else referencing `arm_cfft_*`)
    /// resolve at link time. The library ships inside the Teensyduino
    /// toolchain (`framework-arduinoteensy.../cores/teensy*/`), which the
    /// Teensy linker already adds to the library search path via `-L`.
    /// See FastLED/fbuild#300.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmsis_dsp_lib: Option<String>,
    /// Debug tools from board JSON `debug.tools` section.
    /// Maps tool name (e.g. "simavr", "qemu", "renode") to its metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_tools: Option<HashMap<String, DebugToolMeta>>,
}
