# Library Packages

Arduino library and framework dependency management: spec parsing, download, compilation, and archiving.

## Modules

- **`mod.rs`** -- Module root; re-exports framework types and `LibrarySpec`
- **`library_spec.rs`** -- Parser for `lib_deps` formats (`owner/Name@^version`, GitHub URLs, bare names)
- **`library_downloader.rs`** -- Downloads libraries from GitHub URLs or the PlatformIO registry
- **`library_info.rs`** -- Scans installed libraries for include directories and source files
- **`library_compiler.rs`** -- Compiles library C/C++ sources and archives into static `.a` files
- **`library_manager.rs`** -- Top-level orchestrator: spec parsing, download, discovery, compile, archive
- **`registry.rs`** -- PlatformIO registry API client with semver version constraint resolution
- **`arduino_core.rs`** -- Arduino AVR Core framework package (ArduinoCore-avr from GitHub)
- **`attiny_core.rs`** -- ATTinyCore framework package (SpenceKonde/ATTinyCore)
- **`avr_framework.rs`** -- Data-driven AVR framework resolver using `avr_frameworks.json`
- **`esp32_framework.rs`** -- ESP32 Arduino framework + ESP-IDF precompiled libraries
- **`esp32_platform.rs`** -- ESP32 pioarduino platform package (provides `platform.json` with toolchain metadata)
- **`esp8266_framework.rs`** -- ESP8266 Arduino framework package
- **`teensy_core.rs`** -- Teensy cores framework package (PaulStoffregen/cores)
