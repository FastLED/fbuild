//! Library dependency management.

pub mod arduino_core;
pub mod teensy_core;

pub use arduino_core::ArduinoCore;
pub use teensy_core::TeensyCores;
