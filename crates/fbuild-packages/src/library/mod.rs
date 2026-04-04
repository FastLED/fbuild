//! Library dependency management.

pub mod arduino_core;
pub mod attiny_core;
pub mod avr_framework;
pub mod esp32_framework;
pub mod esp32_platform;
pub mod esp8266_framework;
pub mod library_compiler;
pub mod library_downloader;
pub mod library_info;
pub mod library_manager;
pub mod library_spec;
pub mod nrf52_core;
pub mod registry;
pub mod renesas_core;
pub mod rp2040_core;
pub mod sam_core;
pub mod silabs_core;
pub mod stm32_core;
pub mod teensy_core;

pub use arduino_core::ArduinoCore;
pub use attiny_core::ATTinyCore;
pub use avr_framework::AvrFramework;
pub use esp32_framework::Esp32Framework;
pub use esp32_platform::Esp32Platform;
pub use esp8266_framework::Esp8266Framework;
pub use library_manager::LibraryResult;
pub use library_spec::LibrarySpec;
pub use nrf52_core::Nrf52Cores;
pub use renesas_core::RenesasCores;
pub use rp2040_core::Rp2040Cores;
pub use sam_core::SamCores;
pub use silabs_core::SilabsCores;
pub use stm32_core::Stm32Cores;
pub use teensy_core::TeensyCores;
