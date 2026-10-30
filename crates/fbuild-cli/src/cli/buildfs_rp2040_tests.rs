//! Unit tests for the RP2040 filesystem-region resolver (FastLED/fbuild#1358).
//!
//! Every case goes through the real `boards.txt` loader the RP2040 build uses,
//! on a fixture shaped like arduino-pico's own file, so a change in the loader's
//! menu handling shows up here instead of on hardware.

use super::*;

/// Two `flash` menu options, in arduino-pico's own order: the default 2 MB
/// layout reserves no filesystem, the 512 KB one does.
const BOARDS_TXT: &str = "\
fixture.name=Fixture board
fixture.build.variant=fixture_variant
fixture.menu.flash.2097152_0=2MB (Sketch: 2048KB, FS: 0KB)
fixture.menu.flash.2097152_0.upload.maximum_size=2093056
fixture.menu.flash.2097152_0.build.flash_total=2097152
fixture.menu.flash.2097152_0.build.fs_start=270528512
fixture.menu.flash.2097152_0.build.fs_end=270528512
fixture.menu.flash.2097152_524288=2MB (Sketch: 1536KB, FS: 512KB)
fixture.menu.flash.2097152_524288.upload.maximum_size=1568768
fixture.menu.flash.2097152_524288.build.flash_total=2097152
fixture.menu.flash.2097152_524288.build.fs_start=270004224
fixture.menu.flash.2097152_524288.build.fs_end=270528512
";

fn fixture() -> (tempfile::TempDir, NormalizedPath) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("boards.txt");
    std::fs::write(&path, BOARDS_TXT).unwrap();
    let normalized = NormalizedPath::new(&path);
    (tmp, normalized)
}

/// The stock Pico layout has no filesystem, so `buildfs` must say which knob
/// creates one instead of inventing a region.
#[test]
fn default_menu_without_filesystem_is_a_configuration_error() {
    let (_tmp, boards_txt) = fixture();
    let err = region_from_boards_txt(&boards_txt, "fixture", &HashMap::new()).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("reserves no filesystem space"),
        "{message}"
    );
    assert!(message.contains("board_build.filesystem_size"), "{message}");
}

/// `board_build.filesystem_size` selects the menu option whose reserved
/// region has that size, and the region is reported in absolute flash
/// addresses exactly as the firmware's `FS_START`/`FS_END` defines are.
#[test]
fn filesystem_size_selects_the_matching_flash_menu_option() {
    let (_tmp, boards_txt) = fixture();
    let overrides = HashMap::from([("filesystem_size".to_string(), "0.5m".to_string())]);
    let region = region_from_boards_txt(&boards_txt, "fixture", &overrides).unwrap();
    assert_eq!(region.offset, 270_004_224);
    assert_eq!(region.size, 512 * 1024);
    assert_eq!(region.flash_total, Some(2 * 1024 * 1024));
}

/// An explicit `board_build.flash` menu selection wins over the size-derived
/// one, so a user pinning a geometry keeps it.
#[test]
fn explicit_flash_menu_override_wins() {
    let (_tmp, boards_txt) = fixture();
    let overrides = HashMap::from([("flash".to_string(), "2097152_524288".to_string())]);
    let region = region_from_boards_txt(&boards_txt, "fixture", &overrides).unwrap();
    assert_eq!(region.size, 512 * 1024);
}

/// A board absent from `boards.txt` is a clear error, not a default geometry.
#[test]
fn unknown_board_is_rejected() {
    let (_tmp, boards_txt) = fixture();
    let err = region_from_boards_txt(&boards_txt, "nope", &HashMap::new()).unwrap_err();
    assert!(err.to_string().contains("is not defined in"), "{err}");
}

/// `fs_start == fs_end` is the "no volume" encoding and must never be packed.
#[test]
fn empty_region_from_props_is_rejected() {
    let props = HashMap::from([
        ("fs_start".to_string(), "270528512".to_string()),
        ("fs_end".to_string(), "270528512".to_string()),
    ]);
    let err = region_from_props(&props, "fixture", Path::new("/proj/boards.txt")).unwrap_err();
    assert!(
        err.to_string().contains("reserves no filesystem space"),
        "{err}"
    );
}

/// Missing bounds are reported against the boards.txt that was read.
#[test]
fn missing_bounds_name_the_boards_file() {
    let err =
        region_from_props(&HashMap::new(), "fixture", Path::new("/proj/boards.txt")).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("declares no flash filesystem"),
        "{message}"
    );
    assert!(message.contains("/proj/boards.txt"), "{message}");
}

/// arduino-pico ships one filesystem format; a spiffs request must fail rather
/// than produce a littlefs image the firmware will not mount.
#[test]
fn spiffs_is_refused_on_rp2040() {
    let err = resolve_kind(Some("spiffs"), None).unwrap_err();
    assert!(err.to_string().contains("no spiffs driver"), "{err}");
}

#[test]
fn littlefs_is_the_rp2040_default() {
    assert_eq!(
        resolve_kind(None, None).unwrap(),
        crate::cli::buildfs::FsKind::LittleFs
    );
    assert_eq!(
        resolve_kind(None, Some("littlefs")).unwrap(),
        crate::cli::buildfs::FsKind::LittleFs
    );
}
