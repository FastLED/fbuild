//! RP2040 (arduino-pico) filesystem region for `fbuild buildfs`.
//!
//! arduino-pico has no ESP32-style partition CSV. The framework's own
//! `boards.txt` declares, per `flash` menu option, the absolute bounds of the
//! flash filesystem (`build.fs_start` / `build.fs_end`). `fbuild buildfs` reads
//! those bounds through the same loader the RP2040 build uses
//! (`fbuild_build::arduino_props`), so the image region and the firmware's
//! `FS_START`/`FS_END` defines cannot drift apart (FastLED/fbuild#1358).
//!
//! Only the image-assembly half lives here: flashing an RP2040 filesystem is a
//! PICOBOOT/UF2 operation, not an `esptool` region write, and `fbuild uploadfs`
//! stays ESP32-only until that transport exists.

use std::collections::HashMap;
use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};

/// The PlatformIO package that owns arduino-pico's `boards.txt`.
const FRAMEWORK_PACKAGE: &str = "framework-arduinopico";

/// The flash filesystem region a board's selected `flash` menu option declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rp2040Region {
    /// Absolute flash address of the filesystem (`build.fs_start`).
    pub(crate) offset: u64,
    /// Bytes reserved for the filesystem (`build.fs_end - build.fs_start`).
    pub(crate) size: u64,
    /// Total flash on the selected menu option, when the board declares it.
    pub(crate) flash_total: Option<u64>,
}

/// Turn the loaded `boards.txt` properties of one board into a filesystem
/// region.
///
/// A board that only declares the properties of its *default* menu option is
/// the common case, and arduino-pico's default flash option on a 2 MB Pico
/// reserves no filesystem at all (`fs_start == fs_end`). That is a real
/// configuration, so it is reported as an error naming the knob to change
/// rather than as "partition table not found".
pub(crate) fn region_from_props(
    props: &HashMap<String, String>,
    board_id: &str,
    boards_txt: &Path,
) -> Result<Rp2040Region> {
    let parse = |key: &str| {
        props
            .get(key)
            .and_then(|value| value.trim().parse::<u64>().ok())
    };
    let (Some(start), Some(end)) = (parse("fs_start"), parse("fs_end")) else {
        return Err(FbuildError::ConfigError(format!(
            "board '{board_id}' declares no flash filesystem in {} (no build.fs_start/build.fs_end \
             properties). arduino-pico reserves a filesystem only when the selected `flash` menu \
             option splits the flash; set `board_build.filesystem_size` (for example `0.5m`) for \
             this environment",
            boards_txt.display()
        )));
    };
    if end <= start {
        return Err(FbuildError::ConfigError(format!(
            "board '{board_id}' reserves no filesystem space: build.fs_start 0x{start:x} and \
             build.fs_end 0x{end:x} in {} are the same region. Pick a `flash` menu option that \
             gives the filesystem room (set `board_build.filesystem_size`, for example `0.5m`), \
             otherwise the device has no volume for `data/` to live in",
            boards_txt.display()
        )));
    }
    Ok(Rp2040Region {
        offset: start,
        size: end - start,
        flash_total: parse("flash_total"),
    })
}

/// Resolve a board's filesystem region from a concrete `boards.txt`.
pub(crate) fn region_from_boards_txt(
    boards_txt: &NormalizedPath,
    board_id: &str,
    overrides: &HashMap<String, String>,
) -> Result<Rp2040Region> {
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
    region_from_props(&props, board_id, boards_txt.as_path())
}

/// Candidate `boards.txt` locations: the PlatformIO package directory, then
/// fbuild's own extracted platform cache (`<cache>/platforms/*`).
fn boards_txt_candidates() -> Vec<NormalizedPath> {
    let mut candidates = vec![NormalizedPath::new(
        fbuild_paths::get_platformio_package(FRAMEWORK_PACKAGE).join("boards.txt"),
    )];
    let platform_cache = fbuild_paths::get_cache_root().join("platforms");
    if let Ok(entries) = std::fs::read_dir(platform_cache.as_path()) {
        let mut found: Vec<NormalizedPath> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| NormalizedPath::new(entry.path()).join("boards.txt"))
            .filter(|path| {
                path.as_path()
                    .parent()
                    .and_then(|dir| dir.file_name())
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(FRAMEWORK_PACKAGE))
            })
            .filter(|path| path.is_file())
            .collect();
        found.sort();
        candidates.extend(found);
    }
    candidates
}

/// Locate the arduino-pico `boards.txt` that describes this host's boards.
pub(crate) fn locate_boards_txt() -> Result<NormalizedPath> {
    let candidates = boards_txt_candidates();
    let mut tried = Vec::new();
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate.as_path().display().to_string());
    }
    Err(FbuildError::PackageError(format!(
        "arduino-pico boards.txt not found; checked {}. Install PlatformIO package \
         {FRAMEWORK_PACKAGE} or run `fbuild install`.",
        tried.join(", ")
    )))
}

/// Resolve the filesystem region for `board_id` against the installed
/// arduino-pico framework.
pub(crate) fn resolve_region(
    board_id: &str,
    overrides: &HashMap<String, String>,
) -> Result<Rp2040Region> {
    let boards_txt = locate_boards_txt()?;
    region_from_boards_txt(&boards_txt, board_id, overrides).map_err(|err| match err {
        FbuildError::ConfigError(message) => FbuildError::ConfigError(format!(
            "{message} (framework: {})",
            boards_txt.as_path().display()
        )),
        other => other,
    })
}

/// arduino-pico has a single filesystem format: `--filesystem=spiffs` is a
/// request fbuild cannot honour, and silently building littlefs instead would
/// hand the firmware an image it does not mount.
pub(crate) fn resolve_kind(
    requested: Option<&str>,
    configured: Option<&str>,
) -> Result<crate::cli::buildfs::FsKind> {
    for value in [requested, configured].into_iter().flatten() {
        let kind = crate::cli::buildfs::FsKind::parse(value).ok_or_else(|| {
            FbuildError::ConfigError(format!(
                "unsupported filesystem {value:?}; arduino-pico builds littlefs images only"
            ))
        })?;
        if kind != crate::cli::buildfs::FsKind::LittleFs {
            return Err(FbuildError::ConfigError(format!(
                "arduino-pico has no spiffs driver; {value:?} cannot be flashed to an RP2040. \
                 Use `--filesystem=littlefs` (the default)"
            )));
        }
    }
    Ok(crate::cli::buildfs::FsKind::LittleFs)
}

#[cfg(test)]
#[path = "buildfs_rp2040_tests.rs"]
mod tests;
