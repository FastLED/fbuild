//! Unit tests for `fbuild uploadfs` — the parts that decide what gets flashed
//! where, before any hardware is involved (FastLED/fbuild#1358).

use super::*;

fn args() -> UploadfsArgs {
    UploadfsArgs {
        project_dir: None,
        environment: None,
        data_dir: None,
        partitions: None,
        fs_partition: None,
        filesystem: None,
        tool: None,
        image: None,
        no_build: false,
        port: None,
        baud: None,
        verify: false,
        json: false,
    }
}

/// `uploadfs` must hand `buildfs` exactly the knobs the user set, so a rebuild
/// can never pack a different tree or a different partition than was planned.
#[test]
fn build_args_mirrors_the_upload_flags() {
    let mut upload = args();
    upload.environment = Some("esp32s3".to_string());
    upload.data_dir = Some("assets".to_string());
    upload.partitions = Some("partitions_custom.csv".to_string());
    upload.fs_partition = Some("storage".to_string());
    upload.filesystem = Some("littlefs".to_string());
    upload.tool = Some("/opt/mklittlefs".to_string());
    upload.image = Some("out/fs.bin".to_string());

    let build = upload.build_args();
    assert_eq!(build.environment.as_deref(), Some("esp32s3"));
    assert_eq!(build.data_dir.as_deref(), Some("assets"));
    assert_eq!(build.partitions.as_deref(), Some("partitions_custom.csv"));
    assert_eq!(build.fs_partition.as_deref(), Some("storage"));
    assert_eq!(build.filesystem.as_deref(), Some("littlefs"));
    assert_eq!(build.tool.as_deref(), Some("/opt/mklittlefs"));
    // The upload's `--image` is the build's `--output`: one path, both halves.
    assert_eq!(build.output.as_deref(), Some("out/fs.bin"));
    assert!(!build.check_only, "uploadfs always wants a real image");
}

/// Both esptool platforms have a flash filesystem `uploadfs` can write.
#[test]
fn require_flash_filesystem_accepts_esptool_platforms() {
    assert!(require_flash_filesystem(Some("espressif32"), "esp32s3").is_ok());
    assert!(require_flash_filesystem(Some("espressif8266"), "nodemcuv2").is_ok());
}

/// AVR, Teensy and RP2040 have no filesystem a build tool can flash over
/// esptool; the error is the whole point (the issue asks for a clear failure,
/// never a silent no-op).
#[test]
fn require_flash_filesystem_rejects_platforms_without_one() {
    for platform in ["atmelavr", "teensy", "raspberrypi"] {
        let error = require_flash_filesystem(Some(platform), "uno")
            .expect_err("platform without a flash filesystem must be rejected")
            .to_string();
        assert!(
            error.contains("has no flash filesystem fbuild can write"),
            "unexpected message for {platform}: {error}"
        );
    }
}

#[test]
fn require_flash_filesystem_rejects_a_missing_platform() {
    let error = require_flash_filesystem(None, "mystery")
        .expect_err("a missing platform cannot be flashed")
        .to_string();
    assert!(
        error.contains("needs an ESP32 or ESP8266 environment"),
        "{error}"
    );
}

/// An ESP8266 linker script names the filesystem in the CPU's mapped flash
/// space; esptool wants an offset from the start of the chip, so the mapping
/// comes off. An off-by-one here would write a gigabyte past the end of flash.
#[test]
fn flash_offset_strips_the_esp8266_flash_mapping() {
    let mapped = buildfs_esp8266::FLASH_MAPPED_BASE + 0x200000;
    let offset = flash_offset_for(Some(fbuild_core::Platform::Espressif8266), mapped)
        .expect("mapped filesystem address");
    assert_eq!(offset, 0x200000);
    // The start of the filesystem region is a legal offset of the mapping base.
    assert_eq!(
        flash_offset_for(
            Some(fbuild_core::Platform::Espressif8266),
            buildfs_esp8266::FLASH_MAPPED_BASE
        )
        .expect("region at the mapping base"),
        0
    );
}

/// An address that cannot be a mapped ESP8266 flash address is a hard error, not
/// a negative offset.
#[test]
fn flash_offset_rejects_an_unmapped_esp8266_address() {
    let error = flash_offset_for(Some(fbuild_core::Platform::Espressif8266), 0x1000)
        .expect_err("unmapped address")
        .to_string();
    assert!(error.contains("below the ESP8266 flash mapping"), "{error}");
}

/// ESP32 partition tables already speak flash offsets: they pass through.
#[test]
fn flash_offset_passes_esp32_partition_offsets_through() {
    for platform in [Some(fbuild_core::Platform::Espressif32), None] {
        assert_eq!(
            flash_offset_for(platform, 0x290000).expect("esp32 offset"),
            0x290000
        );
    }
}

/// Port selection must never guess: one port is unambiguous, zero and many are
/// errors that name the candidates.
#[test]
fn choose_port_picks_the_only_detected_port() {
    let chosen = choose_port(&["/dev/ttyACM0".to_string()]).expect("single port");
    assert_eq!(chosen, "/dev/ttyACM0");
}

#[test]
fn choose_port_refuses_to_guess_between_ports() {
    let none = choose_port(&[]).expect_err("no ports").to_string();
    assert!(none.contains("pass --port"), "{none}");

    let many = choose_port(&["/dev/ttyACM0".to_string(), "/dev/ttyUSB0".to_string()])
        .expect_err("ambiguous")
        .to_string();
    assert!(many.contains("2 serial ports detected"), "{many}");
    assert!(many.contains("/dev/ttyACM0, /dev/ttyUSB0"), "{many}");
}

/// `--image` is resolved against the project, like every other path flag.
#[test]
fn absolute_resolves_relative_paths_against_the_project() {
    // Compare components, not display strings: Windows renders the joined
    // path with `\` separators.
    let project = Path::new("/work/blink");
    let relative = absolute(project, "build/littlefs.bin");
    assert!(
        relative
            .as_path()
            .components()
            .eq(Path::new("/work/blink/build/littlefs.bin").components()),
        "{}",
        relative.as_path().display()
    );
    // A host-absolute path is kept as is (`/tmp/..` has no drive on Windows).
    let elsewhere = std::env::temp_dir().join("fs.bin");
    let kept = absolute(project, elsewhere.to_str().expect("utf-8 temp dir"));
    assert!(
        kept.as_path().components().eq(elsewhere.components()),
        "{}",
        kept.as_path().display()
    );
}

/// The argv that actually reaches esptool for an ESP8266 filesystem: `--chip
/// esp8266` and the offset with the `0x40200000` mapping removed, so the write
/// lands inside the chip instead of a gigabyte past the end of flash.
#[test]
fn esp8266_region_argv_targets_the_chip_and_strips_the_mapping() {
    let params = fbuild_deploy::esp32::EsptoolParams {
        flash_mode: "dio".to_string(),
        flash_freq: "40m".to_string(),
        flash_size: "4MB".to_string(),
        default_baud: "115200".to_string(),
        before_reset: "default-reset".to_string(),
        after_reset: "hard-reset".to_string(),
    };
    let deployer = fbuild_deploy::esp32::Esp32Deployer::new(
        "esp8266", "115200", "0x0", "0x0", "0x0", &params, false,
    );
    // A 4M2M layout puts `_FS_start` at 0x40400000, i.e. 2 MB into flash.
    let offset = format!(
        "0x{:x}",
        flash_offset_for(Some(fbuild_core::Platform::Espressif8266), 0x4040_0000)
            .expect("_FS_start")
    );
    assert_eq!(offset, "0x200000");

    let argv = deployer.build_flash_region_args(
        Path::new("/proj/build/littlefs.bin"),
        "/dev/ttyUSB0",
        &offset,
    );
    assert_eq!(
        argv[0], "esptool",
        "unit test runs with no managed tool path"
    );
    let chip = argv.iter().position(|a| a == "--chip").expect("--chip");
    assert_eq!(argv[chip + 1], "esp8266");
    assert!(argv.contains(&"write-flash".to_string()), "{argv:?}");
    assert_eq!(
        &argv[argv.len() - 2..],
        &[offset, "/proj/build/littlefs.bin".to_string()],
        "{argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a.starts_with("0x40")),
        "the CPU-mapped address leaked into esptool argv: {argv:?}"
    );
}
