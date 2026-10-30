//! ESP8266 filesystem region for `fbuild buildfs` (FastLED/fbuild#1358).
//!
//! An ESP8266 sketch has no partition table. The flash layout comes from the
//! `eesz` "Flash Size" menu option in arduino-esp8266's `boards.txt`, which
//! names a linker script — `build.flash_ld=eagle.flash.4m1m.ld`. That script is
//! the authority: the firmware links against its `_FS_start`, `_FS_end`,
//! `_FS_page` and `_FS_block` symbols, so reading the region *and* the
//! filesystem geometry from the same file is the only way the image the host
//! builds cannot drift from the volume the device mounts.
//!
//! `fbuild buildfs` therefore parses the linker script itself rather than
//! re-deriving the layout from `build.spiffs_start` / `build.spiffs_end` (the
//! duplicated values in `boards.txt`) or from the flash-size name.
//!
//! Only the image-assembly half lives here: flashing an ESP8266 is a serial
//! esptool write whose offset is the filesystem address minus the `0x40200000`
//! flash mapping, and `fbuild uploadfs` stays ESP32-only until that transport
//! is added.

use std::collections::HashMap;
use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};

/// The PlatformIO package that owns arduino-esp8266's `boards.txt` and linker
/// scripts.
const FRAMEWORK_PACKAGE: &str = "framework-arduinoespressif8266";

/// The filesystem region and geometry an `eagle.flash.*.ld` declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Esp8266Region {
    /// Absolute address the firmware maps the filesystem at (`_FS_start`).
    pub(crate) offset: u64,
    /// Bytes reserved for the filesystem (`_FS_end - _FS_start`).
    pub(crate) size: u64,
    /// Filesystem block size the firmware erases with (`_FS_block`).
    pub(crate) block: u64,
    /// Filesystem page size (`_FS_page`).
    pub(crate) page: u64,
}

/// Evaluate one `PROVIDE (...)` operand: `0x40500000`, a decimal, or a `+`
/// chain of those. Linker scripts spell the end of a region either as a
/// literal address or as `start + length`; both must resolve to a number.
fn eval_operand(text: &str) -> Option<u64> {
    let mut total: u64 = 0;
    for term in text.split('+') {
        let term = term.trim();
        if term.is_empty() {
            return None;
        }
        let value = match term.strip_prefix("0x").or_else(|| term.strip_prefix("0X")) {
            Some(hex) => u64::from_str_radix(hex.trim_end_matches(['L', 'l']), 16).ok()?,
            None => term.trim_end_matches(['L', 'l']).parse::<u64>().ok()?,
        };
        total = total.checked_add(value)?;
    }
    Some(total)
}

/// Pull the `_FS_*` symbols out of a linker script.
///
/// Only the `PROVIDE ( _FS_start = <expr> );` form arduino-esp8266 ships is
/// understood; anything else is simply not collected, so a script that
/// declares no filesystem reports that instead of guessing.
pub(crate) fn parse_fs_symbols(script: &str) -> HashMap<String, u64> {
    let mut symbols = HashMap::new();
    for line in script.lines() {
        for (start, _) in line.match_indices("_FS_") {
            let rest = &line[start + 4..];
            let Some((name, tail)) = rest.split_once('=') else {
                continue;
            };
            let value = tail.split(';').next().unwrap_or(tail);
            let value = value.trim().trim_end_matches(')').trim();
            if let Some(number) = eval_operand(value) {
                symbols.insert(format!("_FS_{}", name.trim()), number);
            }
        }
    }
    symbols
}

/// Turn one `eagle.flash.*.ld` into a region.
pub(crate) fn region_from_ld(script: &str, ld_path: &Path) -> Result<Esp8266Region> {
    let symbols = parse_fs_symbols(script);
    let get = |name: &str| symbols.get(name).copied();
    let (Some(start), Some(end)) = (get("_FS_start"), get("_FS_end")) else {
        return Err(FbuildError::ConfigError(format!(
            "{} declares no flash filesystem: the linker script has no _FS_start/_FS_end \
             symbols. Pick a `Flash Size` menu option that reserves space for a filesystem \
             (`board_build.eesz`, for example `4M1M`)",
            ld_path.display()
        )));
    };
    if end <= start {
        return Err(FbuildError::ConfigError(format!(
            "{} reserves no filesystem space: _FS_start 0x{start:x} and _FS_end 0x{end:x} \
             describe an empty region. Pick a `Flash Size` menu option that gives the \
             filesystem room (`board_build.eesz`)",
            ld_path.display()
        )));
    }
    // The image tool must format with the firmware's own block/page size, so a
    // script that omits them is a hard error rather than a silent 4096/256.
    let (Some(block), Some(page)) = (get("_FS_block"), get("_FS_page")) else {
        return Err(FbuildError::ConfigError(format!(
            "{} declares _FS_start/_FS_end but no _FS_block/_FS_page; the image cannot be \
             formatted without the firmware's own filesystem geometry",
            ld_path.display()
        )));
    };
    if block == 0 || page == 0 || block % page != 0 {
        return Err(FbuildError::ConfigError(format!(
            "{} declares an unusable filesystem geometry: _FS_block 0x{block:x}, \
             _FS_page 0x{page:x}",
            ld_path.display()
        )));
    }
    Ok(Esp8266Region {
        offset: start,
        size: end - start,
        block,
        page,
    })
}

/// Resolve the region for `board_id` from a concrete `boards.txt` plus the
/// framework root that holds `tools/sdk/ld/`.
pub(crate) fn region_from_boards_txt(
    boards_txt: &NormalizedPath,
    framework_root: &NormalizedPath,
    board_id: &str,
    overrides: &HashMap<String, String>,
) -> Result<Esp8266Region> {
    let props = fbuild_build::arduino_props::load_board_props_with_menu_overrides(
        boards_txt.as_path(),
        board_id,
        overrides,
    )
    .ok_or_else(|| {
        FbuildError::ConfigError(format!(
            "board '{board_id}' is not defined in {}; check the `board` value in platformio.ini",
            boards_txt.as_path().display()
        ))
    })?;
    let flash_ld = overrides
        .get("flash_ld")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| props.get("flash_ld").map(|value| value.trim().to_string()))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            FbuildError::ConfigError(format!(
                "board '{board_id}' in {} declares no build.flash_ld; without the linker \
                 script there is no filesystem region to size an image from. Set \
                 `board_build.eesz` (Flash Size) to a menu option that reserves one",
                boards_txt.as_path().display()
            ))
        })?;
    let ld_path = framework_root
        .join("tools")
        .join("sdk")
        .join("ld")
        .join(&flash_ld);
    let script = std::fs::read_to_string(ld_path.as_path()).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to read the esp8266 linker script {}: {e}",
            ld_path.as_path().display()
        ))
    })?;
    region_from_ld(&script, ld_path.as_path())
}

/// Candidate arduino-esp8266 roots: the PlatformIO package directory, then
/// fbuild's own extracted platform cache (`<cache>/platforms/*`).
fn framework_candidates() -> Vec<NormalizedPath> {
    let mut candidates = vec![NormalizedPath::new(fbuild_paths::get_platformio_package(
        FRAMEWORK_PACKAGE,
    ))];
    let platform_cache = fbuild_paths::get_cache_root().join("platforms");
    if let Ok(entries) = std::fs::read_dir(platform_cache.as_path()) {
        let mut found: Vec<NormalizedPath> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| NormalizedPath::new(entry.path()))
            .filter(|path| {
                path.as_path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(FRAMEWORK_PACKAGE))
            })
            .filter(|path| path.join("tools").join("sdk").join("ld").is_dir())
            .collect();
        found.sort();
        candidates.extend(found);
    }
    candidates
}

/// Locate the arduino-esp8266 framework directory on this host.
pub(crate) fn locate_framework() -> Result<NormalizedPath> {
    let mut tried = Vec::new();
    for candidate in framework_candidates() {
        if candidate.join("boards.txt").is_file() {
            return Ok(candidate);
        }
        tried.push(candidate.as_path().display().to_string());
    }
    Err(FbuildError::PackageError(format!(
        "arduino-esp8266 framework not found; checked {}. Install PlatformIO package \
         {FRAMEWORK_PACKAGE} or run `fbuild install`.",
        tried.join(", ")
    )))
}

/// Resolve the filesystem region for `board_id` against the installed
/// arduino-esp8266 framework.
pub(crate) fn resolve_region(
    board_id: &str,
    overrides: &HashMap<String, String>,
) -> Result<Esp8266Region> {
    let root = locate_framework()?;
    let boards_txt = root.join("boards.txt");
    region_from_boards_txt(&boards_txt, &root, board_id, overrides).map_err(|err| match err {
        FbuildError::ConfigError(message) => FbuildError::ConfigError(format!(
            "{message} (framework: {})",
            boards_txt.as_path().display()
        )),
        other => other,
    })
}

/// arduino-esp8266 ships both filesystems; LittleFS is the default (SPIFFS is
/// deprecated upstream) and the explicit request always wins.
pub(crate) fn resolve_kind(
    requested: Option<&str>,
    configured: Option<&str>,
) -> Result<crate::cli::buildfs::FsKind> {
    match requested.or(configured) {
        Some(value) => crate::cli::buildfs::FsKind::parse(value).ok_or_else(|| {
            FbuildError::ConfigError(format!(
                "unsupported filesystem {value:?}; esp8266 builds littlefs or spiffs images"
            ))
        }),
        None => Ok(crate::cli::buildfs::FsKind::LittleFs),
    }
}

#[cfg(test)]
#[path = "buildfs_esp8266_tests.rs"]
mod tests;
