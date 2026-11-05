# Library Packages

Arduino library and framework dependency management: spec parsing, download, compilation, and archiving.

## Modules

- **`mod.rs`** -- Module root; re-exports framework types and `LibrarySpec`
- **`library_spec.rs`** -- Parser for `lib_deps` formats (`owner/Name@^version`, GitHub URLs, bare names, and named `symlink://`/`file://` local dependencies)
- **`library_downloader.rs`** -- Downloads libraries from GitHub URLs or the PlatformIO registry
- **`library_info.rs`** -- Scans installed libraries for include directories and source files
- **`library_compiler.rs`** -- Compiles library C/C++ sources and archives into static `.a` files. `.c`, `.S` (assembler-with-cpp) and `.s` (raw assembly) go through the C driver with the target C flags; the remaining collected extensions (`.cpp`/`.cc`/`.cxx`) go through C++. The rebuild signature is computed from that same choice, so changing a driver invalidates the object instead of silently reusing it (FastLED/fbuild#1885).
- **`library_dispatch_tests.rs`** -- The driver/flag/signature contract for library TUs, kept in its own module so the main test file stays under the per-file cap
- **`tu_history.rs`** -- Per-TU compile duration history under the cache root, used to dispatch the longest translation units first (FastLED/fbuild#1564)
- **`library_manager.rs`** -- Top-level orchestrator: spec parsing, download, discovery, compile, archive
- **`registry.rs`** -- PlatformIO registry API client with semver version constraint resolution
- **`arduino_core.rs`** -- Arduino AVR Core framework package (ArduinoCore-avr from GitHub)
- **`samd_core.rs`** -- Adafruit ArduinoCore-samd framework package
- **`clearcore_core.rs`** -- Teknic ClearCore Arduino package for ATSAME53
- **`attiny_core.rs`** -- ATTinyCore framework package (SpenceKonde/ATTinyCore)
- **`avr_framework.rs`** -- Data-driven AVR framework resolver using `avr_frameworks.json`
- **`esp32_framework.rs`** -- ESP32 Arduino framework + ESP-IDF precompiled libraries
- **`esp32_platform.rs`** -- ESP32 pioarduino platform package (provides `platform.json` with toolchain metadata)
- **`esp8266_framework.rs`** -- ESP8266 Arduino framework package
- **`teensy_core.rs`** -- Teensy cores framework package (PaulStoffregen/cores)

Manifest-aware discovery uses fallible include/source APIs. `build.srcDir`, when
explicitly supplied, must name an existing directory; a missing path or regular
file is a configuration error, rather than an apparently header-only library.
When omitted, the existing `src/src`, `src`, then library-root layout fallback
remains. A valid existing directory containing headers but no translation units
is still a legitimate header-only library. String/array `build.srcFilter` rules
are applied in order relative to that selected source directory; malformed
metadata or rules propagate as configuration errors to build callers.

The legacy `get_source_files`, `get_include_dirs` and `is_header_only` methods
remain unfiltered diagnostic views. Production builders use fallible filtered
selection instead, including explicit dependencies, extra/project-local
libraries, project-as-library builds and framework dependency seeds. Project
self-build/collision guards remain, using the resolved manifest source root;
malformed metadata cannot be reclassified as a skipped/header-only library.

Project-as-library example compilers and dependency searches receive the same
validated manifest source/include roots before compiler creation. Adding a root
only to the archive compiler is insufficient: the example's own include flags
and dependency graph must also resolve that library's public headers.
