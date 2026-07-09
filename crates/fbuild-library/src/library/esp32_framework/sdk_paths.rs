//! SDK include, library, define, and linker flag accessors for the ESP-IDF SDK
//! shipped with the ESP32 Arduino framework.

use std::path::{Path, PathBuf};

use super::fs_utils::{collect_archive_files, scan_include_dirs_recursive};
use super::parsing::{parse_include_flags, split_defines};
use super::Esp32Framework;

/// Get the SDK directory for a given MCU.
///
/// Tries new layout (`tools/esp32-arduino-libs/{mcu}`) first, falls back to
/// old layout (`tools/sdk/{mcu}`).
pub(super) fn sdk_mcu_dir(fw: &Esp32Framework, mcu: &str) -> PathBuf {
    let root = fw.resolved_dir();
    let new_path = root.join("tools").join("esp32-arduino-libs").join(mcu);
    if new_path.exists() {
        return new_path;
    }
    root.join("tools").join("sdk").join(mcu)
}

fn sdk_memory_variant_dir(sdk_dir: &Path, requested: Option<&str>) -> Option<PathBuf> {
    if let Some(requested) = requested {
        let requested_dir = sdk_dir.join(requested);
        if requested_dir.exists() {
            return Some(requested_dir);
        }
    }

    for variant in &["qio_opi", "dio_opi", "opi_opi", "qio_qspi", "dio_qspi"] {
        let candidate = sdk_dir.join(variant);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

impl Esp32Framework {
    /// Get the SDK directory for a given MCU.
    ///
    /// Tries new layout (`tools/esp32-arduino-libs/{mcu}`) first, falls back to
    /// old layout (`tools/sdk/{mcu}`).
    fn sdk_mcu_dir(&self, mcu: &str) -> PathBuf {
        sdk_mcu_dir(self, mcu)
    }

    /// Get SDK include directories for a given MCU.
    ///
    /// Reads the `flags/includes` file from the SDK directory, which lists
    /// all 305+ include paths. Falls back to scanning `include/` subdirectories.
    pub fn get_sdk_include_dirs(&self, mcu: &str, memory_type: Option<&str>) -> Vec<PathBuf> {
        let root = self.resolved_dir();
        let sdk_dir = self.sdk_mcu_dir(mcu);

        // Try reading the includes list file (supports both -I and -iwithprefixbefore formats)
        let includes_file = sdk_dir.join("flags").join("includes");
        if includes_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&includes_file) {
                let include_base = sdk_dir.join("include");
                let mut dirs = parse_include_flags(&content, &include_base, &root);

                // Add flash/PSRAM variant include dir (contains sdkconfig.h).
                // Try common variants in preference order; the correct one depends
                // on board_build.flash_mode and board_build.arduino.memory_type.
                if let Some(variant_dir) = sdk_memory_variant_dir(&sdk_dir, memory_type) {
                    let v_include = variant_dir.join("include");
                    if v_include.exists() {
                        dirs.push(v_include);
                    }
                }

                return dirs;
            }
        }

        // Fallback: recursively scan include/ subdirectories.
        // The 2.x framework (PlatformIO-compat) has deeply nested includes
        // under tools/sdk/{mcu}/include/ (e.g., freertos/include/freertos,
        // freertos/port/xtensa/include).
        let include_dir = sdk_dir.join("include");
        if !include_dir.exists() {
            return Vec::new();
        }

        let mut dirs = Vec::new();

        // Prepend newlib/platform_include (provides assert.h, errno.h, time.h)
        // which must come before SDK headers. PlatformIO also puts this first.
        let newlib_platform = include_dir.join("newlib").join("platform_include");
        if newlib_platform.exists() {
            dirs.push(newlib_platform);
        }

        // Scan 4 levels deep — matches PlatformIO's actual include depth.
        // ESP-IDF components have nested includes up to 4 levels deep
        // (e.g., freertos/include/esp_additions/freertos/).
        scan_include_dirs_recursive(&include_dir, &mut dirs, 0, 4);

        // Add well-known ESP-IDF Xtensa/RISC-V port include paths that the
        // scanner misses because headers are nested too deeply for detection.
        for sub_mcu in &["esp32", "esp32s2", "esp32s3"] {
            let xtensa_inc = include_dir.join("xtensa").join(sub_mcu).join("include");
            if xtensa_inc.exists() && !dirs.contains(&xtensa_inc) {
                dirs.push(xtensa_inc);
            }
        }

        // Also add flash/PSRAM variant include dir (contains sdkconfig.h).
        if let Some(variant_dir) = sdk_memory_variant_dir(&sdk_dir, memory_type) {
            let v_include = variant_dir.join("include");
            if v_include.exists() {
                dirs.push(v_include);
            }
        }

        dirs.sort();
        dirs
    }

    /// Get all precompiled `.a` library files from the ESP-IDF SDK.
    pub fn get_sdk_libs(&self, mcu: &str) -> Vec<PathBuf> {
        let lib_dir = self.sdk_mcu_dir(mcu).join("lib");
        collect_archive_files(&lib_dir)
    }

    /// Get the ordered SDK linker library flags from `flags/ld_libs`.
    ///
    /// Returns the pre-ordered `-l` flags (with duplicates for circular deps)
    /// as specified by the SDK. Falls back to scanning `lib/` for `.a` files
    /// if the flags file doesn't exist.
    pub fn get_sdk_lib_flags(&self, mcu: &str, memory_type: Option<&str>) -> Vec<String> {
        let sdk_dir = self.sdk_mcu_dir(mcu);
        let ld_libs_file = sdk_dir.join("flags").join("ld_libs");

        if let Ok(content) = std::fs::read_to_string(&ld_libs_file) {
            let mut flags = vec![format!("-L{}", sdk_dir.join("lib").display())];
            // Add ld/ directory as a library search path
            let ld_dir = sdk_dir.join("ld");
            if ld_dir.exists() {
                flags.push(format!("-L{}", ld_dir.display()));
            }
            // Add flash-mode-specific directory (contains libspi_flash.a and others).
            // Default to dio_qspi (most common for ESP32dev boards).
            if let Some(variant_dir) = sdk_memory_variant_dir(&sdk_dir, memory_type) {
                flags.push(format!("-L{}", variant_dir.display()));
            }
            flags.extend(fbuild_core::shell_split::split(&content));
            return flags;
        }

        // Fallback: scan lib/ directory for .a files
        let lib_dir = sdk_dir.join("lib");
        let mut flags = Vec::new();
        if lib_dir.exists() {
            flags.push(format!("-L{}", lib_dir.display()));
        }
        for lib in collect_archive_files(&lib_dir) {
            if let Some(stem) = lib.file_stem() {
                let name = stem.to_string_lossy();
                if let Some(stripped) = name.strip_prefix("lib") {
                    flags.push(format!("-l{}", stripped));
                }
            }
        }
        flags
    }

    /// Get the SDK compiler defines from `flags/defines`.
    ///
    /// Returns `-D` flags that must be passed to the compiler for SDK headers
    /// to work correctly (e.g., `MBEDTLS_CONFIG_FILE`, `IDF_VER`).
    /// Returns empty if the flags file doesn't exist.
    ///
    /// Uses `split_defines` instead of `shell_split` because define values
    /// like `-DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\"` contain escaped
    /// quotes that must be preserved for GCC.
    pub fn get_sdk_defines(&self, mcu: &str) -> Vec<String> {
        let defines_file = self.sdk_mcu_dir(mcu).join("flags").join("defines");
        if let Ok(content) = std::fs::read_to_string(&defines_file) {
            return split_defines(&content);
        }
        Vec::new()
    }

    /// Get the ordered SDK linker flags from `flags/ld_flags`.
    ///
    /// Returns the linker flags (undefined symbols, wrap directives, etc.)
    /// as specified by the SDK. Returns empty if the flags file doesn't exist.
    pub fn get_sdk_ld_flags(&self, mcu: &str) -> Vec<String> {
        let ld_flags_file = self.sdk_mcu_dir(mcu).join("flags").join("ld_flags");
        if let Ok(content) = std::fs::read_to_string(&ld_flags_file) {
            return fbuild_core::shell_split::split(&content);
        }
        Vec::new()
    }

    /// Get the SDK linker script flags from `flags/ld_scripts`.
    ///
    /// Returns the `-T` flags in the correct order, with the ld directory
    /// as the search path. Falls back to the ld/ directory if no flags file.
    pub fn get_sdk_ld_scripts(&self, mcu: &str) -> Vec<String> {
        let sdk_dir = self.sdk_mcu_dir(mcu);
        let ld_scripts_file = sdk_dir.join("flags").join("ld_scripts");

        let mut flags = vec![format!("-L{}", sdk_dir.join("ld").display())];

        if let Ok(content) = std::fs::read_to_string(&ld_scripts_file) {
            flags.extend(fbuild_core::shell_split::split(&content));
            return flags;
        }

        // Fallback: no scripts
        flags
    }
}
