//! ESP32 Arduino framework package.
//!
//! Downloads and manages the Arduino-ESP32 core + ESP-IDF precompiled libraries.
//! This combines what PlatformIO splits into two packages:
//! - `framework-arduinoespressif32`: Arduino core, variants, libraries
//! - `framework-arduinoespressif32-libs`: ESP-IDF SDK includes + precompiled `.a` libs
//!
//! Key methods provide paths to:
//! - Core sources: `cores/esp32/`
//! - Board variants: `variants/{mcu}/`
//! - SDK include dirs: `tools/sdk/{mcu}/include/` (305+ paths)
//! - SDK precompiled libs: `tools/sdk/{mcu}/lib/` (100+ .a files)
//! - Linker scripts: `tools/sdk/{mcu}/ld/`
//! - Bootloader/partitions: `tools/sdk/{mcu}/bin/`

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const ESP32_FRAMEWORK_VERSION: &str = "3.1.1";
const ESP32_FRAMEWORK_URL: &str =
    "https://github.com/pioarduino/arduino-esp32/releases/download/3.1.1/framework-arduinoespressif32-3.1.1.tar.gz";

/// ESP32 Arduino framework manager.
pub struct Esp32Framework {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl Esp32Framework {
    /// Create with hardcoded URL (legacy, for tests).
    pub fn new(project_dir: &Path, _mcu: &str) -> Self {
        Self {
            base: PackageBase::new(
                "esp32-arduino",
                ESP32_FRAMEWORK_VERSION,
                ESP32_FRAMEWORK_URL,
                ESP32_FRAMEWORK_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
            ),
            install_dir: None,
        }
    }

    /// Create from a resolved URL (from platform.json).
    ///
    /// The orchestrator reads `platform.json` → `packages.framework-arduinoespressif32.version`
    /// to get the correct download URL (e.g. espressif/arduino-esp32 release).
    pub fn from_url(project_dir: &Path, url: &str) -> Self {
        // Extract version from URL (e.g., "3.3.7" from ".../3.3.7/esp32-core-3.3.7.tar.xz")
        let version = extract_framework_version(url);

        Self {
            base: PackageBase::new(
                "esp32-arduino",
                &version,
                url,
                "framework-arduinoespressif32",
                None,
                CacheSubdir::Platforms,
                project_dir,
            ),
            install_dir: None,
        }
    }

    /// Ensure the SDK libs are downloaded and extracted into the framework's `tools/` dir.
    pub fn ensure_libs(&self, libs_url: &str) -> fbuild_core::Result<()> {
        let root = self.resolved_dir();
        let tools_dir = root.join("tools");

        // Already have SDK libs? Check both old (sdk/) and new (esp32-arduino-libs/) layouts
        for dir_name in &["esp32-arduino-libs", "sdk"] {
            let sdk_dir = tools_dir.join(dir_name);
            if sdk_dir.exists() && sdk_dir.is_dir() {
                if let Ok(mut entries) = std::fs::read_dir(&sdk_dir) {
                    if entries.next().is_some() {
                        return Ok(());
                    }
                }
            }
        }

        std::fs::create_dir_all(&tools_dir)?;

        // Check for already-downloaded archive (skip re-download)
        let archive_filename = libs_url.rsplit('/').next().unwrap_or("libs.tar.xz");
        let archive_path = tools_dir.join(archive_filename);

        if !archive_path.exists() {
            tracing::info!("downloading ESP32 SDK libs");
            let rt = tokio::runtime::Handle::try_current().ok();
            if let Some(handle) = rt {
                handle.block_on(crate::downloader::download_file(libs_url, &tools_dir))?;
            } else {
                let rt = tokio::runtime::Runtime::new().map_err(|e| {
                    fbuild_core::FbuildError::PackageError(format!(
                        "failed to create tokio runtime: {}",
                        e
                    ))
                })?;
                rt.block_on(crate::downloader::download_file(libs_url, &tools_dir))?;
            }
        }

        // Extract to a short temp path to avoid Windows MAX_PATH (260 char) limit.
        // Then rename (atomic on same filesystem) to final location.
        let temp_dir = std::env::temp_dir().join(format!("fbuild_sdk_{}", std::process::id()));
        if temp_dir.exists() {
            let _ = std::fs::remove_dir_all(&temp_dir);
        }
        std::fs::create_dir_all(&temp_dir)?;

        tracing::info!(
            "extracting ESP32 SDK libs ({} MB)",
            archive_path
                .metadata()
                .map(|m| m.len() / 1_000_000)
                .unwrap_or(0)
        );
        crate::extractor::extract(&archive_path, &temp_dir)?;
        let _ = std::fs::remove_file(&archive_path);

        // Move extracted content to final tools/ dir (same filesystem = fast rename)
        if let Ok(entries) = std::fs::read_dir(&temp_dir) {
            for entry in entries.flatten() {
                let src = entry.path();
                let dest = tools_dir.join(entry.file_name());
                if dest.exists() {
                    let _ = std::fs::remove_dir_all(&dest);
                }
                if std::fs::rename(&src, &dest).is_err() {
                    copy_dir_recursive(&src, &dest)?;
                }
            }
        }
        let _ = std::fs::remove_dir_all(&temp_dir);

        tracing::info!("ESP32 SDK libs installed");
        Ok(())
    }

    /// Get the resolved root directory of the framework.
    fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_framework_root(&self.base.install_path()))
    }

    /// Validate the extracted framework has required structure.
    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let root = find_framework_root(install_dir);

        let cores_dir = root.join("cores").join("esp32");
        if !cores_dir.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "ESP32 framework missing cores/esp32/ directory (in {})",
                root.display()
            )));
        }

        let arduino_h = cores_dir.join("Arduino.h");
        if !arduino_h.exists() {
            return Err(fbuild_core::FbuildError::PackageError(
                "ESP32 framework missing cores/esp32/Arduino.h".to_string(),
            ));
        }

        Ok(())
    }

    /// Get the core source directory (e.g. `cores/esp32`).
    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        self.resolved_dir().join("cores").join(core_name)
    }

    /// Get the variant directory for a board (e.g. `variants/esp32c6`).
    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.resolved_dir().join("variants").join(variant_name)
    }

    /// Get the SDK directory for a given MCU.
    ///
    /// Tries new layout (`tools/esp32-arduino-libs/{mcu}`) first, falls back to
    /// old layout (`tools/sdk/{mcu}`).
    fn sdk_mcu_dir(&self, mcu: &str) -> PathBuf {
        let root = self.resolved_dir();
        let new_path = root.join("tools").join("esp32-arduino-libs").join(mcu);
        if new_path.exists() {
            return new_path;
        }
        root.join("tools").join("sdk").join(mcu)
    }

    /// Get SDK include directories for a given MCU.
    ///
    /// Reads the `flags/includes` file from the SDK directory, which lists
    /// all 305+ include paths. Falls back to scanning `include/` subdirectories.
    pub fn get_sdk_include_dirs(&self, mcu: &str) -> Vec<PathBuf> {
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
                let variants = ["qio_opi", "dio_opi", "opi_opi", "qio_qspi", "dio_qspi"];
                for variant in &variants {
                    let v_include = sdk_dir.join(variant).join("include");
                    if v_include.exists() {
                        dirs.push(v_include);
                        break;
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
        let variants = ["qio_opi", "dio_opi", "opi_opi", "qio_qspi", "dio_qspi"];
        for variant in &variants {
            let v_include = sdk_dir.join(variant).join("include");
            if v_include.exists() {
                dirs.push(v_include);
                break;
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
    pub fn get_sdk_lib_flags(&self, mcu: &str) -> Vec<String> {
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
            for flash_mode in &["dio_qspi", "qio_qspi"] {
                let fm_dir = sdk_dir.join(flash_mode);
                if fm_dir.exists() {
                    flags.push(format!("-L{}", fm_dir.display()));
                    break;
                }
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
    /// Uses [`split_defines`] instead of `shell_split` because define values
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

    /// Get the linker scripts directory for a given MCU.
    pub fn get_linker_scripts_dir(&self, mcu: &str) -> PathBuf {
        self.sdk_mcu_dir(mcu).join("ld")
    }

    /// Get the path to the bootloader binary.
    ///
    /// First checks for a pre-built `bootloader.bin`. If not found, looks for
    /// `bootloader_{flash_mode}_{flash_freq}.elf` which needs elf2image conversion.
    pub fn get_bootloader_bin(&self, mcu: &str) -> PathBuf {
        self.sdk_mcu_dir(mcu).join("bin").join("bootloader.bin")
    }

    /// Get the path to the bootloader ELF for a given flash mode and frequency.
    ///
    /// ESP32 Arduino core provides pre-built bootloader ELFs named
    /// `bootloader_{mode}_{freq}.elf`. The ROM bootloader on ESP32-S3 and
    /// similar chips can only load the second-stage bootloader in DIO mode,
    /// so `flash_mode` should typically be "dio" regardless of application mode.
    pub fn get_bootloader_elf(&self, mcu: &str, flash_mode: &str, flash_freq: &str) -> PathBuf {
        let filename = format!("bootloader_{}_{}.elf", flash_mode, flash_freq);
        self.sdk_mcu_dir(mcu).join("bin").join(filename)
    }

    /// Get the path to the partitions binary.
    pub fn get_partitions_bin(&self, mcu: &str) -> PathBuf {
        self.sdk_mcu_dir(mcu).join("bin").join("partitions.bin")
    }

    /// Get the path to the partitions CSV file.
    pub fn get_partitions_csv(&self, partitions_name: &str) -> PathBuf {
        self.resolved_dir()
            .join("tools")
            .join("partitions")
            .join(partitions_name)
    }

    /// Get the path to gen_esp32part.py for generating partition tables.
    pub fn get_gen_esp32part(&self) -> PathBuf {
        self.resolved_dir().join("tools").join("gen_esp32part.py")
    }

    /// List all source files in a core directory.
    pub fn get_core_sources(&self, core_name: &str) -> Vec<PathBuf> {
        collect_sources(&self.get_core_dir(core_name))
    }
}

impl crate::Package for Esp32Framework {
    fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let rt = tokio::runtime::Handle::try_current().ok();
        let install_path = if let Some(handle) = rt {
            handle.block_on(self.base.staged_install(Self::validate))?
        } else {
            let rt = tokio::runtime::Runtime::new().map_err(|e| {
                fbuild_core::FbuildError::PackageError(format!(
                    "failed to create tokio runtime: {}",
                    e
                ))
            })?;
            rt.block_on(self.base.staged_install(Self::validate))?
        };

        Ok(find_framework_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_framework_root(&self.base.install_path());
        root.join("cores").join("esp32").join("Arduino.h").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for Esp32Framework {
    fn get_cores_dir(&self) -> PathBuf {
        self.resolved_dir().join("cores")
    }

    fn get_variants_dir(&self) -> PathBuf {
        self.resolved_dir().join("variants")
    }

    fn get_libraries_dir(&self) -> PathBuf {
        self.resolved_dir().join("libraries")
    }
}

/// Parse include flags from the `flags/includes` file.
///
/// Uses `fbuild_core::shell_split::split` to tokenize (handles quoted paths,
/// safe on Windows). Iterates with an index, consuming flag+path pairs.
/// Split a defines string into individual `-D` flags, preserving escaped quotes.
///
/// The `flags/defines` file contains flags like:
/// ```text
/// -DFOO=1 -DBAR=\"baz.h\" -DQUX
/// ```
/// The `\"` sequences must be preserved because GCC needs the quotes in
/// define values (e.g., `MBEDTLS_CONFIG_FILE` expands to `"mbedtls/esp_config.h"`).
///
/// Unlike `shell_split`, this splits on whitespace boundaries that precede `-D`
/// and keeps the raw content of each flag intact.
fn split_defines(content: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    // Split on " -D" boundaries (preserving the -D prefix)
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return tokens;
    }
    // Find each -D token boundary
    let mut start = 0;
    let bytes = trimmed.as_bytes();
    let len = bytes.len();
    let mut i = 1; // skip first char
    while i < len {
        // A new -D token starts when we see whitespace followed by -D
        if bytes[i] == b'-'
            && i + 1 < len
            && bytes[i + 1] == b'D'
            && i > 0
            && bytes[i - 1].is_ascii_whitespace()
        {
            let token = trimmed[start..i].trim();
            if !token.is_empty() {
                tokens.push(token.to_string());
            }
            start = i;
        }
        i += 1;
    }
    // Last token
    let token = trimmed[start..].trim();
    if !token.is_empty() {
        tokens.push(token.to_string());
    }
    tokens
}

/// Handles two flag formats:
/// - `-iwithprefixbefore relative/path` (new 3.3.7+, resolved against include_base)
/// - `-I/absolute/path` or `-Irelative/path` (legacy 3.1.x)
fn parse_include_flags(content: &str, include_base: &Path, root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let parts = fbuild_core::shell_split::split(content);
    let mut i = 0;
    while i < parts.len() {
        if parts[i] == "-iwithprefixbefore" {
            if i + 1 < parts.len() {
                let resolved = include_base.join(&parts[i + 1]);
                if resolved.exists() {
                    dirs.push(resolved);
                }
                i += 2;
            } else {
                i += 1;
            }
        } else if let Some(path_str) = parts[i].strip_prefix("-I") {
            if !path_str.is_empty() {
                let p = if Path::new(path_str).is_absolute() {
                    PathBuf::from(path_str)
                } else {
                    root.join(path_str)
                };
                if p.exists() {
                    dirs.push(p);
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    dirs
}

/// Extract a version string from a framework URL.
///
/// E.g., `".../download/3.3.7/esp32-core-3.3.7.tar.xz"` → `"3.3.7"`
fn extract_framework_version(url: &str) -> String {
    // Look for a path segment that is purely a version number (digits + dots)
    for segment in url.rsplit('/') {
        let s = segment
            .trim_end_matches(".tar.xz")
            .trim_end_matches(".tar.gz")
            .trim_end_matches(".zip");
        if s.chars().all(|c| c.is_ascii_digit() || c == '.') && s.contains('.') && !s.is_empty() {
            return s.to_string();
        }
    }
    // Fallback: hash
    crate::cache::hash_url(url)
}

/// Find the actual framework root inside an extracted archive.
/// Recursively copy a directory tree.
fn copy_dir_recursive(src: &Path, dest: &Path) -> fbuild_core::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else {
            std::fs::copy(&src_path, &dest_path)?;
        }
    }
    Ok(())
}

fn find_framework_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("cores").exists() {
        return install_dir.to_path_buf();
    }

    // Check one level deep
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("cores").exists() {
                return path;
            }
        }
    }

    install_dir.to_path_buf()
}

/// Recursively scan for include paths that are useful for compilation.
///
/// A directory is added if it contains headers directly OR has an immediate
/// child directory with headers (supporting `#include "subdir/header.h"`).
/// This matches PlatformIO's include strategy without adding every directory.
///
/// Used for the 2.x framework layout where `flags/includes` doesn't exist.
fn scan_include_dirs_recursive(
    dir: &Path,
    dirs: &mut Vec<PathBuf>,
    depth: usize,
    max_depth: usize,
) {
    if depth > max_depth {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut has_headers = false;
        let mut child_has_headers = false;
        let mut subdirs = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Check if this child dir has headers (one level peek)
                if !child_has_headers && dir_contains_headers(&path) {
                    child_has_headers = true;
                }
                subdirs.push(path);
            } else if !has_headers {
                if let Some(ext) = path.extension() {
                    if ext == "h" || ext == "hpp" {
                        has_headers = true;
                    }
                }
            }
        }

        // Add if dir has headers or a child dir has headers
        if has_headers || child_has_headers {
            dirs.push(dir.to_path_buf());
        }

        for subdir in subdirs {
            scan_include_dirs_recursive(&subdir, dirs, depth + 1, max_depth);
        }
    }
}

/// Check if a directory directly contains any header files.
fn dir_contains_headers(dir: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "h" || ext == "hpp" {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Collect all `.a` archive files from a directory (non-recursive).
fn collect_archive_files(dir: &Path) -> Vec<PathBuf> {
    let mut libs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "a") {
                libs.push(path);
            }
        }
    }
    libs.sort();
    libs
}

/// Collect source files from a directory (non-recursive).
fn collect_sources(dir: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let ext = path
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                if matches!(ext.as_str(), "c" | "cpp" | "cc" | "s") {
                    sources.push(path);
                }
            }
        }
    }
    sources.sort();
    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn test_esp32_framework_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("FBUILD_CACHE_DIR", tmp.path().join("cache"));
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        assert!(!fw.is_installed());
        std::env::remove_var("FBUILD_CACHE_DIR");
    }

    #[test]
    fn test_find_framework_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores")).unwrap();
        assert_eq!(find_framework_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_framework_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("framework-arduinoespressif32");
        std::fs::create_dir_all(nested.join("cores")).unwrap();
        assert_eq!(find_framework_root(tmp.path()), nested);
    }

    #[test]
    fn test_get_core_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let core_dir = fw.get_core_dir("esp32");
        assert!(core_dir.to_string_lossy().contains("cores"));
        assert!(core_dir.to_string_lossy().contains("esp32"));
    }

    #[test]
    fn test_get_variant_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let variant_dir = fw.get_variant_dir("esp32c6");
        assert!(variant_dir.to_string_lossy().contains("variants"));
        assert!(variant_dir.to_string_lossy().contains("esp32c6"));
    }

    #[test]
    fn test_sdk_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let ld_dir = fw.get_linker_scripts_dir("esp32c6");
        assert!(ld_dir.to_string_lossy().contains("sdk"));
        assert!(ld_dir.to_string_lossy().contains("esp32c6"));
        assert!(ld_dir.to_string_lossy().contains("ld"));
    }

    #[test]
    fn test_collect_archive_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("libfreertos.a"), "").unwrap();
        std::fs::write(tmp.path().join("libesp_system.a"), "").unwrap();
        std::fs::write(tmp.path().join("readme.txt"), "").unwrap();
        let libs = collect_archive_files(tmp.path());
        assert_eq!(libs.len(), 2);
        assert!(libs.iter().all(|p| p.extension().unwrap() == "a"));
    }

    #[test]
    fn test_get_sdk_libs_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let libs = fw.get_sdk_libs("esp32c6");
        assert!(libs.is_empty()); // No SDK installed
    }

    #[test]
    fn test_validate_missing_cores() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = Esp32Framework::validate(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_missing_arduino_h() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores").join("esp32")).unwrap();
        let result = Esp32Framework::validate(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Arduino.h"));
    }

    #[test]
    fn test_bootloader_bin_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let boot = fw.get_bootloader_bin("esp32c6");
        assert!(boot.to_string_lossy().contains("bootloader.bin"));
    }

    #[test]
    fn test_partitions_bin_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let parts = fw.get_partitions_bin("esp32c6");
        assert!(parts.to_string_lossy().contains("partitions.bin"));
    }

    #[test]
    fn test_parse_iwithprefixbefore_format() {
        let tmp = tempfile::TempDir::new().unwrap();
        let include_base = tmp.path().join("include");

        // Create dirs that match the relative paths
        let freertos = include_base.join("freertos/include/freertos");
        let esp_sys = include_base.join("esp_system/include");
        std::fs::create_dir_all(&freertos).unwrap();
        std::fs::create_dir_all(&esp_sys).unwrap();

        // This is the actual format from flags/includes files
        let content =
            "-iwithprefixbefore freertos/include/freertos -iwithprefixbefore esp_system/include";
        let dirs = parse_include_flags(content, &include_base, tmp.path());

        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs[0], freertos);
        assert_eq!(dirs[1], esp_sys);
    }

    #[test]
    fn test_sdk_include_dirs_with_mock() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Create mock SDK structure with includes file
        let sdk_dir = tmp.path().join("tools").join("sdk").join("esp32c6");
        let flags_dir = sdk_dir.join("flags");
        std::fs::create_dir_all(&flags_dir).unwrap();

        // Create some include dirs
        let inc1 = sdk_dir.join("include").join("freertos");
        let inc2 = sdk_dir.join("include").join("esp_system");
        std::fs::create_dir_all(&inc1).unwrap();
        std::fs::create_dir_all(&inc2).unwrap();

        // Write includes file with absolute paths
        let includes_content = format!("-I{}\n-I{}\n", inc1.display(), inc2.display());
        std::fs::write(flags_dir.join("includes"), &includes_content).unwrap();

        let fw = Esp32Framework {
            base: PackageBase::new(
                "test",
                "1.0",
                "http://example.com",
                "http://example.com",
                None,
                CacheSubdir::Platforms,
                tmp.path(),
            ),
            install_dir: Some(tmp.path().to_path_buf()),
        };

        let dirs = fw.get_sdk_include_dirs("esp32c6");
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn test_split_defines_preserves_escaped_quotes() {
        let content =
            r#"-DFOO=1 -DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\" -DBAR -DIDF_VER=\"v5.5.2\""#;
        let tokens = split_defines(content);
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0], "-DFOO=1");
        assert_eq!(
            tokens[1],
            r#"-DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\""#
        );
        assert_eq!(tokens[2], "-DBAR");
        assert_eq!(tokens[3], r#"-DIDF_VER=\"v5.5.2\""#);
    }

    #[test]
    fn test_split_defines_empty() {
        assert!(split_defines("").is_empty());
        assert!(split_defines("   ").is_empty());
    }

    #[test]
    fn test_split_defines_single() {
        assert_eq!(split_defines("-DFOO=1"), vec!["-DFOO=1"]);
    }
}
