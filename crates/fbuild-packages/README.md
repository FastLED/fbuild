# fbuild-packages

URL-based package management with cached downloads, checksum verification, and staged installation for toolchains, frameworks, and libraries.

## Key Types

- `Package` -- Base trait for installable packages (`ensure_installed`, `is_installed`, `get_info`)
- `Toolchain` -- Trait extending `Package` with GCC/G++/AR/objcopy/size tool paths
- `Framework` -- Trait extending `Package` with cores/variants/libraries directory paths
- `PackageBase` -- Shared implementation handling download, SHA256 verification, extraction, and atomic staged install
- `Cache` -- URL-hashed directory manager for packages, toolchains, platforms, libraries, and per-project build artifacts
- `PackageInfo` -- Package metadata (name, version, URL, install path)

## Modules

- **cache** -- Cache directory layout with stem/hash naming for human-readable browsing
- **downloader** -- Async HTTP downloads with SHA256 checksum verification and parallel batch support
- **extractor** -- Pure-Rust archive extraction (tar.gz, tar.bz2, tar.xz, tar.zst, zip)
- **library** -- Arduino library dependency management: spec parsing, download, compilation, archiving
- **toolchain** -- Platform-specific toolchain packages (AVR-GCC, ARM GCC, ESP32 Xtensa/RISC-V, ESP8266, Clang)
