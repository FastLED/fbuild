//! Library dependency management.

pub mod arduino_core;
pub mod esp32_framework;
pub mod esp32_platform;
pub mod library_compiler;
pub mod library_downloader;
pub mod library_info;
pub mod library_manager;
pub mod library_spec;
pub mod registry;
pub mod teensy_core;

pub use arduino_core::ArduinoCore;
pub use esp32_framework::Esp32Framework;
pub use esp32_platform::Esp32Platform;
pub use library_manager::LibraryResult;
pub use library_spec::LibrarySpec;
pub use teensy_core::TeensyCores;
