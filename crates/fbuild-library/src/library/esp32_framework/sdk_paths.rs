//! SDK include, library, define, and linker flag accessors for the ESP-IDF SDK
//! shipped with the ESP32 Arduino framework.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::Esp32Framework;
use super::fs_utils::{collect_archive_files, scan_include_dirs_recursive};
use super::parsing::{parse_include_flags, parse_pio_cppdefines, split_defines};

/// A parsed builder script yielding fewer entries than this is treated as a
/// failed parse. An upstream format change must degrade to the tree scan,
/// never to a silently truncated include path.
const MIN_PIO_CPPPATH_ENTRIES: usize = 20;

/// Resolve a `join(FRAMEWORK_DIR, "tools", "sdk", ...)` call from the
/// framework's PlatformIO builder script into a path relative to the
/// framework root. Returns `None` when the call is anything other than plain
/// string literals (e.g. one computed from `env.BoardConfig()`).
fn pio_join_literals(line: &str) -> Option<PathBuf> {
    let mut path = PathBuf::new();
    let mut rest = line;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let end = after.find('"')?;
        path.push(&after[..end]);
        rest = &after[end + 1..];
    }
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(path)
}

/// Read the include list out of the framework's own PlatformIO builder script
/// (`tools/platformio-build-<mcu>.py`).
///
/// SDK layouts that predate `flags/includes` (arduino-esp32 2.x) have no
/// machine-readable include list, but they do ship the SCons builder that
/// PlatformIO uses for exactly this framework version, and its `CPPPATH`
/// block is that list, in PlatformIO's order.
///
/// Reconstructing the list by scanning the tree instead is wrong in both
/// directions: the depth cap misses leaves PlatformIO passes (e.g.
/// `bt/common/api/include/api`, `lwip/port/esp32/include/arch`) while still
/// emitting hundreds of dirs PlatformIO never passes. Every extra `-I` costs
/// a failed path lookup on every unresolved `#include`, which measured ~41%
/// per translation unit on ESP32-S3 (FastLED/fbuild#1537).
///
/// Returns `None` when the script is absent or does not parse, so the caller
/// can fall back to the tree scan.
fn parse_pio_cpppath(root: &Path, mcu: &str) -> Option<Vec<PathBuf>> {
    let per_mcu_script = root
        .join("tools")
        .join(format!("platformio-build-{mcu}.py"));
    let script = if per_mcu_script.exists() {
        per_mcu_script
    } else {
        root.join("tools").join("platformio-build.py")
    };
    let content = std::fs::read_to_string(&script).ok()?;

    let mut dirs = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if !in_block {
            if trimmed.starts_with("CPPPATH=[") || trimmed.starts_with("CPPPATH = [") {
                in_block = true;
            }
            continue;
        }
        if trimmed.starts_with(']') {
            break;
        }
        if !trimmed.contains("join(FRAMEWORK_DIR") || trimmed.contains("env.") {
            continue;
        }
        if let Some(rel) = pio_join_literals(trimmed) {
            let resolved = root.join(rel);
            if resolved.exists() {
                dirs.push(resolved);
            }
        }
    }

    // Dedupe before applying the threshold: the count that matters is distinct
    // paths, so a script that repeats one entry enough times cannot pass the
    // guard and then dedupe down to a truncated include path.
    let mut seen = HashSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()));

    if dirs.len() < MIN_PIO_CPPPATH_ENTRIES {
        return None;
    }

    Some(dirs)
}

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
    let flat_path = root.join("tools").join("sdk");
    if flat_path.join("include").exists() {
        return flat_path;
    }
    flat_path.join(mcu)
}

fn sdk_memory_variant_dir(sdk_dir: &Path, requested: Option<&str>) -> Option<PathBuf> {
    if let Some(requested) = requested {
        let requested_dir = sdk_dir.join(requested);
        if requested_dir.exists() {
            return Some(requested_dir);
        }
    }

    for variant in &[
        "qio_opi",
        "dio_opi",
        "opi_opi",
        "qio_qspi",
        "dio_qspi",
        "qspi_qspi",
    ] {
        let candidate = sdk_dir.join(variant);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

fn sdk_include_order(a: &Path, b: &Path, root: &Path) -> Ordering {
    let a = a.strip_prefix(root).unwrap_or(a);
    let b = b.strip_prefix(root).unwrap_or(b);
    for (left, right) in a.components().zip(b.components()) {
        let left = left.as_os_str();
        let right = right.as_os_str();
        if left == right {
            continue;
        }
        if left == "include" {
            return Ordering::Less;
        }
        if right == "include" {
            return Ordering::Greater;
        }
        return left.cmp(right);
    }
    a.components().count().cmp(&b.components().count())
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

        // Old-layout SDK (arduino-esp32 2.x) with no `flags/includes`: use the
        // include list from the framework's own PlatformIO builder script.
        if let Some(mut dirs) = parse_pio_cpppath(&root, mcu) {
            // The flash/PSRAM variant entry is computed from board config in
            // the script, so the caller supplies it (as it does above).
            if let Some(variant_dir) = sdk_memory_variant_dir(&sdk_dir, memory_type) {
                let v_include = variant_dir.join("include");
                if v_include.exists() && !dirs.contains(&v_include) {
                    dirs.push(v_include);
                }
            }
            return dirs;
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
            dirs.push(newlib_platform.clone());
        }

        // Scan 4 levels deep — matches PlatformIO's actual include depth.
        // ESP-IDF components have nested includes up to 4 levels deep
        // (e.g., freertos/include/esp_additions/freertos/).
        scan_include_dirs_recursive(&include_dir, &mut dirs, 0, 4);
        // `time.h` in newlib/platform_include uses `#include_next` to reach
        // the compiler's libc header. Treating its `sys/` child as a second
        // `-I` directory makes that lookup land on `sys/time.h` instead.
        dirs.retain(|dir| dir == &newlib_platform || !dir.starts_with(&newlib_platform));

        // Old SDK bundles ROM headers for several chips. Adding every ROM
        // subdirectory makes `#include "rom/gpio.h"` pick ESP32's definition
        // before ESP32-S3's, unlike the selected platform recipe.
        let rom_root = include_dir.join("esp_rom");
        let allowed_rom_dirs = [
            rom_root.join("include"),
            rom_root.join("include").join(mcu),
            rom_root.join(mcu),
        ];
        dirs.retain(|dir| {
            !dir.starts_with(&rom_root) || allowed_rom_dirs.iter().any(|allowed| dir == allowed)
        });

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

        dirs.sort_by(|a, b| sdk_include_order(a, b, &include_dir));
        dirs.dedup();
        if newlib_platform.exists() {
            dirs.retain(|dir| dir != &newlib_platform);
            dirs.insert(0, newlib_platform);
        }
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

        // Old 2.x SDKs have no flags/ld_libs. Their selected flash-memory
        // variant overrides several common archives (notably FreeRTOS and
        // esp_system), so it must be searched before the common lib/ dir.
        let lib_dir = sdk_dir.join("lib");
        let mut flags = Vec::new();
        let variant_dir = sdk_memory_variant_dir(&sdk_dir, memory_type);
        if let Some(variant_dir) = &variant_dir {
            flags.push(format!("-L{}", variant_dir.display()));
        }
        if lib_dir.exists() {
            flags.push(format!("-L{}", lib_dir.display()));
        }
        let mut libraries = variant_dir
            .as_ref()
            .map(|dir| collect_archive_files(dir))
            .unwrap_or_default();
        libraries.extend(collect_archive_files(&lib_dir));
        let mut names = std::collections::BTreeSet::new();
        for lib in libraries {
            if let Some(stem) = lib.file_stem() {
                let name = stem.to_string_lossy();
                if let Some(stripped) = name.strip_prefix("lib") {
                    names.insert(stripped.to_string());
                }
            }
        }
        flags.extend(names.into_iter().map(|name| format!("-l{name}")));
        flags
    }

    /// Get the SDK compiler defines from `flags/defines`.
    ///
    /// Returns `-D` flags that must be passed to the compiler for SDK headers
    /// to work correctly (e.g., `MBEDTLS_CONFIG_FILE`, `IDF_VER`). SDK layouts
    /// without `flags/defines` (arduino-esp32 2.x) fall back to the
    /// `CPPDEFINES` block of the framework's PlatformIO builder script; empty
    /// when neither exists.
    ///
    /// Uses `split_defines` instead of `shell_split` because define values
    /// like `-DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\"` contain escaped
    /// quotes that must be preserved for GCC.
    pub fn get_sdk_defines(&self, mcu: &str) -> Vec<String> {
        let defines_file = self.sdk_mcu_dir(mcu).join("flags").join("defines");
        if let Ok(content) = std::fs::read_to_string(&defines_file) {
            return split_defines(&content);
        }
        let script = self
            .resolved_dir()
            .join("tools")
            .join(format!("platformio-build-{mcu}.py"));
        std::fs::read_to_string(script)
            .map(|content| parse_pio_cppdefines(&content))
            .unwrap_or_default()
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
    pub fn get_sdk_ld_scripts(&self, mcu: &str, memory_type: Option<&str>) -> Vec<String> {
        let sdk_dir = self.sdk_mcu_dir(mcu);
        let ld_scripts_file = sdk_dir.join("flags").join("ld_scripts");

        let mut flags = vec![format!("-L{}", sdk_dir.join("ld").display())];
        if let Some(variant_dir) = sdk_memory_variant_dir(&sdk_dir, memory_type) {
            flags.push(format!("-L{}", variant_dir.display()));
        }

        if let Ok(content) = std::fs::read_to_string(&ld_scripts_file) {
            flags.extend(fbuild_core::shell_split::split(&content));
            return flags;
        }

        // Fallback: no scripts
        flags
    }
}

#[cfg(test)]
mod legacy_sdk_tests {
    use super::*;

    #[test]
    fn platformio_1_11_2_builder_supplies_flat_sdk_includes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let mut script = String::from("CPPPATH=[\n");
        for i in 0..20 {
            let dir = root.join(format!("tools/sdk/include/component{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            script.push_str(&format!(
                "join(FRAMEWORK_DIR, \"tools\", \"sdk\", \"include\", \"component{i}\"),\n"
            ));
        }
        script.push_str("]\n");
        std::fs::write(root.join("tools/platformio-build.py"), script).unwrap();

        let dirs = parse_pio_cpppath(root, "esp32").unwrap();
        assert_eq!(dirs.len(), 20);
        assert_eq!(dirs[0], root.join("tools/sdk/include/component0"));
    }
}
