//! Unit tests for the ESP8266 filesystem-region resolver (FastLED/fbuild#1358).
//!
//! The fixtures are cut down from arduino-esp8266's own `boards.txt` and
//! `tools/sdk/ld/eagle.flash.4m1m.ld`, and every case goes through the real
//! `boards.txt` menu loader, so a change in the loader's `eesz` handling shows
//! up here instead of on a device.

use super::*;

/// Two `eesz` options in arduino-esp8266's own order: the 1 MB layout reserves
/// a 64 KB filesystem, the 4 MB one a 1 MB filesystem.
const BOARDS_TXT: &str = "\
fixture.name=Fixture module
fixture.build.board=ESP8266_FIXTURE
fixture.build.mcu=esp8266
fixture.menu.eesz.1M64=1MB (FS:64KB OTA:~470KB)
fixture.menu.eesz.1M64.build.flash_size=1M
fixture.menu.eesz.1M64.build.flash_ld=eagle.flash.1m64.ld
fixture.menu.eesz.4M1M=4MB (FS:1MB OTA:~1019KB)
fixture.menu.eesz.4M1M.build.flash_size=4M
fixture.menu.eesz.4M1M.build.flash_ld=eagle.flash.4m1m.ld
fixture.menu.xtal.80=80 MHz
fixture.menu.xtal.80.build.f_cpu=80000000L
";

/// `eagle.flash.4m1m.ld`'s filesystem half, verbatim.
const LD_4M1M: &str = "\
MEMORY
{
  irom0_0_seg :                         org = 0x40201010, len = 0xfeff0
}

PROVIDE ( _FS_start = 0x40500000 );
PROVIDE ( _FS_end = 0x405FA000 );
PROVIDE ( _FS_page = 0x100 );
PROVIDE ( _FS_block = 0x2000 );
PROVIDE ( _EEPROM_start = 0x405fb000 );
/* The following symbols are DEPRECATED and will be REMOVED in a future release */
PROVIDE ( _SPIFFS_start = 0x40500000 );
PROVIDE ( _SPIFFS_end = 0x405FA000 );
PROVIDE ( _SPIFFS_page = 0x100 );
PROVIDE ( _SPIFFS_block = 0x2000 );
";

/// `eagle.flash.1m64.ld`'s filesystem half.
const LD_1M64: &str = "\
PROVIDE ( _FS_start = 0x40200000 + 0xFB000 );
PROVIDE ( _FS_end = 0x40200000 + 0x10B000 );
PROVIDE ( _FS_page = 0x100 );
PROVIDE ( _FS_block = 0x1000 );
";

fn fixture(scripts: &[(&str, &str)]) -> (tempfile::TempDir, NormalizedPath, NormalizedPath) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("framework");
    std::fs::create_dir_all(root.join("tools/sdk/ld")).unwrap();
    std::fs::write(root.join("boards.txt"), BOARDS_TXT).unwrap();
    for (name, body) in scripts {
        std::fs::write(root.join("tools/sdk/ld").join(name), body).unwrap();
    }
    let boards_txt = NormalizedPath::new(root.join("boards.txt"));
    (tmp, NormalizedPath::new(root), boards_txt)
}

fn region(overrides: &[(&str, &str)]) -> Result<Esp8266Region> {
    let scripts = [
        ("eagle.flash.1m64.ld", LD_1M64),
        ("eagle.flash.4m1m.ld", LD_4M1M),
    ];
    let (_tmp, root, boards_txt) = fixture(&scripts);
    let overrides = overrides
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<_, _>>();
    region_from_boards_txt(&boards_txt, &root, "fixture", &overrides)
}

/// The image region is whatever the linker script declares, in the absolute
/// addresses the firmware's `_FS_start`/`_FS_end` symbols carry.
#[test]
fn default_menu_region_comes_from_the_linker_script() {
    let region = region(&[]).unwrap();
    assert_eq!(region.offset, 0x402F_B000);
    assert_eq!(region.size, 64 * 1024);
    // geometry is the script's own, not the ESP32 builder's 4096/256
    assert_eq!(region.block, 4096);
    assert_eq!(region.page, 256);
}

/// `board_build.eesz` selects the menu option, and with it a different linker
/// script — the region follows the selection rather than the board default.
#[test]
fn eesz_override_selects_the_flash_menu_script() {
    let region = region(&[("eesz", "4M1M")]).unwrap();
    assert_eq!(region.offset, 0x4050_0000);
    assert_eq!(region.size, 0xFA000);
    assert_eq!(region.block, 0x2000);
    assert_eq!(region.page, 0x100);
}

/// A `+` expression in the script resolves, so `_FS_end = start + length`
/// layouts report the same region a literal would.
#[test]
fn sum_expressions_in_the_script_are_evaluated() {
    let symbols = parse_fs_symbols(LD_1M64);
    assert_eq!(symbols["_FS_start"], 0x402F_B000);
    assert_eq!(symbols["_FS_end"], 0x4030_B000);
}

/// The deprecated `_SPIFFS_*` aliases must not be mistaken for `_FS_*`.
#[test]
fn deprecated_spiffs_aliases_are_not_collected() {
    let symbols = parse_fs_symbols(LD_4M1M);
    assert!(
        !symbols.keys().any(|key| key.contains("SPIFFS")),
        "{symbols:?}"
    );
    assert_eq!(symbols["_FS_start"], 0x4050_0000);
}

/// A script with no filesystem is a configuration error naming the knob, not a
/// default region.
#[test]
fn script_without_filesystem_names_the_knob() {
    let err = region_from_ld(
        "PROVIDE ( _EEPROM_start = 0x405fb000 );\n",
        Path::new("/ld/x.ld"),
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("declares no flash filesystem"),
        "{message}"
    );
    assert!(message.contains("board_build.eesz"), "{message}");
}

/// An empty region (`_FS_start == _FS_end`) must never be packed.
#[test]
fn empty_region_is_rejected() {
    let script = "PROVIDE ( _FS_start = 0x40500000 );\nPROVIDE ( _FS_end = 0x40500000 );\n\
                  PROVIDE ( _FS_page = 0x100 );\nPROVIDE ( _FS_block = 0x2000 );\n";
    let err = region_from_ld(script, Path::new("/ld/x.ld")).unwrap_err();
    assert!(
        err.to_string().contains("reserves no filesystem space"),
        "{err}"
    );
}

/// Without `_FS_block`/`_FS_page` the image cannot be formatted the way the
/// firmware mounts it, so the resolver refuses instead of defaulting.
#[test]
fn missing_geometry_is_rejected() {
    let script = "PROVIDE ( _FS_start = 0x40500000 );\nPROVIDE ( _FS_end = 0x405FA000 );\n";
    let err = region_from_ld(script, Path::new("/ld/x.ld")).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("no _FS_block/_FS_page"), "{message}");
}

/// A block that is not a whole number of pages cannot be formatted.
#[test]
fn inconsistent_geometry_is_rejected() {
    let script = "PROVIDE ( _FS_start = 0x40500000 );\nPROVIDE ( _FS_end = 0x405FA000 );\n\
                  PROVIDE ( _FS_page = 0x300 );\nPROVIDE ( _FS_block = 0x800 );\n";
    let err = region_from_ld(script, Path::new("/ld/x.ld")).unwrap_err();
    assert!(
        err.to_string().contains("unusable filesystem geometry"),
        "{err}"
    );
}

/// `board_build.flash_ld` pins the script itself, overriding the menu option.
#[test]
fn explicit_flash_ld_override_wins() {
    let region = region(&[("eesz", "1M64"), ("flash_ld", "eagle.flash.4m1m.ld")]).unwrap();
    assert_eq!(region.size, 0xFA000);
}

/// A board that is not in `boards.txt` is a clear error, not a default.
#[test]
fn unknown_board_is_rejected() {
    let (_tmp, root, boards_txt) = fixture(&[("eagle.flash.4m1m.ld", LD_4M1M)]);
    let err = region_from_boards_txt(&boards_txt, &root, "nope", &HashMap::new()).unwrap_err();
    assert!(err.to_string().contains("is not defined in"), "{err}");
}

/// A board whose menu option names no `flash_ld` cannot be sized.
#[test]
fn board_without_flash_ld_is_rejected() {
    let props_free = "\
fixture.name=No layout
fixture.build.board=ESP8266_FIXTURE
";
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("framework");
    std::fs::create_dir_all(root.join("tools/sdk/ld")).unwrap();
    std::fs::write(root.join("boards.txt"), props_free).unwrap();
    let err = region_from_boards_txt(
        &NormalizedPath::new(root.join("boards.txt")),
        &NormalizedPath::new(&root),
        "fixture",
        &HashMap::new(),
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("no build.flash_ld"), "{message}");
    assert!(message.contains("board_build.eesz"), "{message}");
}

/// arduino-esp8266 ships both filesystems; littlefs is the default and an
/// explicit request is honoured.
#[test]
fn filesystem_kind_defaults_to_littlefs() {
    use crate::cli::buildfs::FsKind;
    assert_eq!(resolve_kind(None, None).unwrap(), FsKind::LittleFs);
    assert_eq!(resolve_kind(None, Some("spiffs")).unwrap(), FsKind::Spiffs);
    assert_eq!(
        resolve_kind(Some("littlefs"), None).unwrap(),
        FsKind::LittleFs
    );
    let err = resolve_kind(Some("fatfs"), None).unwrap_err();
    assert!(err.to_string().contains("unsupported filesystem"), "{err}");
}

/// The geometry drives the tool argv, so an 8192/256 volume is not formatted
/// with the ESP32's 4096/256.
#[test]
fn geometry_drives_the_image_tool_argv() {
    let geometry = crate::cli::buildfs_geometry::FsGeometry::from_parts(8192, 256);
    let argv = geometry.tool_args(
        Path::new("/proj/fs_stage"),
        1_024_000,
        Path::new("/proj/.pio/build/fixture/littlefs.bin"),
    );
    assert_eq!(argv[0], "-c");
    assert_eq!(argv[1], "/proj/fs_stage");
    assert_eq!(&argv[2..6], ["-b", "8192", "-p", "256"]);
    assert_eq!(&argv[6..8], ["-s", "1024000"]);
}
