//! Library dependency management.

pub mod arduino_core;
pub mod esp32_framework;
pub mod teensy_core;

pub use arduino_core::ArduinoCore;
pub use esp32_framework::Esp32Framework;
pub use teensy_core::TeensyCores;
